//! Durable, owner-and-relay-scoped project turn admission.
//!
//! This store is an enforcement primitive only. Callers must reserve before
//! dispatch and release only after they know the exact worker generation has
//! stopped. It deliberately has no lease timeout or self-recovery heuristic.

use std::{fs, fs::OpenOptions, path::PathBuf};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

use super::{scoped_db_file_for, validate_project_coordinate, validate_turn_id};

const MAX_PROJECT_POLICIES: usize = 4_096;
const MAX_RETAINED_RELEASED_LEASES: usize = 8_192;
const RELEASED_LEASE_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

pub struct ProjectAdmissionJournal {
    db_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectReserveOutcome {
    /// There is no configured finite cap, so this primitive creates no lease.
    NotLimited,
    /// A new lease was committed before dispatch.
    Admitted { active: u32, limit: u32 },
    /// The exact turn and worker generation already own an active lease.
    AlreadyReserved { active: u32, limit: Option<u32> },
    /// The configured limit is already occupied.
    AtCapacity { active: u32, limit: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectLeaseRelease {
    Released,
    AlreadyReleased,
    UnknownTurn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectAdmissionStatus {
    pub max_active: Option<u32>,
    pub active: u32,
}

impl ProjectAdmissionJournal {
    /// Open a local store isolated by Buzz nest, relay, and workspace owner.
    pub fn open_scoped(
        nest_dir: impl AsRef<std::path::Path>,
        relay_url: &str,
        owner_pubkey: &str,
    ) -> Result<Self, String> {
        let db_file = scoped_db_file_for("project-admission", relay_url, owner_pubkey)?;
        let nest_dir = nest_dir.as_ref();
        if nest_dir
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("Buzz nest is a symlink; refusing project admission path".into());
        }
        fs::create_dir_all(nest_dir).map_err(|error| format!("create Buzz nest: {error}"))?;
        let journal_dir = nest_dir.join("project-admission-journals");
        if journal_dir
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("Buzz project admission directory is a symlink".into());
        }
        fs::create_dir_all(&journal_dir)
            .map_err(|error| format!("create Buzz project admission directory: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&journal_dir, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("secure Buzz project admission directory: {error}"))?;
        }
        let journal = Self {
            db_path: journal_dir.join(db_file),
        };
        let conn = journal.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_admission_policy (
                project_coordinate TEXT PRIMARY KEY,
                max_active INTEGER CHECK(max_active IS NULL OR max_active > 0),
                updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
            );
            CREATE TABLE IF NOT EXISTS project_turn_leases (
                turn_id TEXT PRIMARY KEY,
                project_coordinate TEXT NOT NULL,
                generation_nonce TEXT NOT NULL,
                reserved_at_ms INTEGER NOT NULL CHECK(reserved_at_ms >= 0),
                released_at_ms INTEGER CHECK(released_at_ms IS NULL OR released_at_ms >= reserved_at_ms)
            );
            CREATE INDEX IF NOT EXISTS project_turn_leases_active
                ON project_turn_leases(project_coordinate, released_at_ms);",
        )
        .map_err(|error| format!("initialize project admission journal: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&journal.db_path, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("secure Buzz project admission file: {error}"))?;
        }
        Ok(journal)
    }

    /// Set a positive finite project limit. Lowering below current activity is
    /// allowed; the returned count makes that state visible and blocks new work.
    pub fn set_limit(
        &self,
        project_coordinate: &str,
        max_active: u32,
        now_ms: i64,
    ) -> Result<u32, String> {
        let project_coordinate = canonical_project_coordinate(project_coordinate)?;
        if max_active == 0 {
            return Err("project turn limit must be positive".into());
        }
        validate_timestamp(now_ms)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin project limit update: {error}"))?;
        ensure_policy_capacity(&tx, &project_coordinate, MAX_PROJECT_POLICIES)?;
        tx.execute(
            "INSERT INTO project_admission_policy(project_coordinate, max_active, updated_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_coordinate) DO UPDATE SET
                max_active=excluded.max_active, updated_at_ms=excluded.updated_at_ms",
            params![project_coordinate, max_active, now_ms],
        )
        .map_err(|error| format!("save project turn limit: {error}"))?;
        let active = active_count(&tx, &project_coordinate)?;
        tx.commit()
            .map_err(|error| format!("commit project turn limit: {error}"))?;
        Ok(active)
    }

    /// Clear a finite limit without discarding active leases needed for exact
    /// generation release or idempotent retries.
    pub fn clear_limit(&self, project_coordinate: &str, now_ms: i64) -> Result<(), String> {
        let project_coordinate = canonical_project_coordinate(project_coordinate)?;
        validate_timestamp(now_ms)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin project limit clear: {error}"))?;
        ensure_policy_capacity(&tx, &project_coordinate, MAX_PROJECT_POLICIES)?;
        tx.execute(
            "INSERT INTO project_admission_policy(project_coordinate, max_active, updated_at_ms)
             VALUES (?1, NULL, ?2)
             ON CONFLICT(project_coordinate) DO UPDATE SET
                max_active=NULL, updated_at_ms=excluded.updated_at_ms",
            params![project_coordinate, now_ms],
        )
        .map_err(|error| format!("clear project turn limit: {error}"))?;
        tx.commit()
            .map_err(|error| format!("commit project limit clear: {error}"))?;
        Ok(())
    }

    /// Read the current project cap and active lease count from one snapshot.
    pub fn status(&self, project_coordinate: &str) -> Result<ProjectAdmissionStatus, String> {
        let project_coordinate = canonical_project_coordinate(project_coordinate)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|error| format!("begin project admission status read: {error}"))?;
        let status = ProjectAdmissionStatus {
            max_active: configured_limit(&tx, &project_coordinate)?,
            active: active_count(&tx, &project_coordinate)?,
        };
        tx.commit()
            .map_err(|error| format!("finish project admission status read: {error}"))?;
        Ok(status)
    }

    /// Atomically reserve capacity before opening an ACP session or sending a
    /// prompt. SQLite's immediate transaction prevents concurrent overbooking.
    pub fn reserve_turn(
        &self,
        project_coordinate: &str,
        turn_id: &str,
        generation_nonce: Option<&str>,
        now_ms: i64,
    ) -> Result<ProjectReserveOutcome, String> {
        let project_coordinate = canonical_project_coordinate(project_coordinate)?;
        let turn_id = canonical_turn_id(turn_id)?;
        if let Some(generation_nonce) = generation_nonce {
            validate_generation_nonce(generation_nonce)?;
        }
        validate_timestamp(now_ms)?;

        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin project turn reservation: {error}"))?;
        let prior: Option<(String, String, Option<i64>)> = tx
            .query_row(
                "SELECT project_coordinate, generation_nonce, released_at_ms
                 FROM project_turn_leases WHERE turn_id=?1",
                [&turn_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("read existing project turn lease: {error}"))?;
        if let Some((prior_project, prior_nonce, released_at_ms)) = prior {
            let generation_nonce = generation_nonce
                .ok_or("worker generation is required to resume an existing project lease")?;
            if prior_project != project_coordinate || prior_nonce != generation_nonce {
                return Err(
                    "managed turn ID is already bound to another project generation".into(),
                );
            }
            if released_at_ms.is_some() {
                return Err("managed turn lease was already released".into());
            }
            let limit = configured_limit(&tx, &project_coordinate)?;
            let active = active_count(&tx, &project_coordinate)?;
            tx.commit()
                .map_err(|error| format!("finish idempotent project reservation: {error}"))?;
            return Ok(ProjectReserveOutcome::AlreadyReserved { active, limit });
        }

        let Some(limit) = configured_limit(&tx, &project_coordinate)? else {
            tx.commit()
                .map_err(|error| format!("finish unlimited project admission: {error}"))?;
            return Ok(ProjectReserveOutcome::NotLimited);
        };
        let generation_nonce = generation_nonce
            .ok_or("worker generation is required for a finite project turn limit")?;
        let active = active_count(&tx, &project_coordinate)?;
        if active >= limit {
            return Ok(ProjectReserveOutcome::AtCapacity { active, limit });
        }
        tx.execute(
            "INSERT INTO project_turn_leases
             (turn_id, project_coordinate, generation_nonce, reserved_at_ms, released_at_ms)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![&turn_id, project_coordinate, generation_nonce, now_ms],
        )
        .map_err(|error| format!("save project turn lease: {error}"))?;
        let active = active.saturating_add(1);
        tx.commit()
            .map_err(|error| format!("commit project turn reservation: {error}"))?;
        Ok(ProjectReserveOutcome::Admitted { active, limit })
    }

    /// Release all leases for one process generation after its exact process
    /// receipt has been verified stopped by the caller.
    pub fn release_generation_after_exit(
        &self,
        generation_nonce: &str,
        now_ms: i64,
    ) -> Result<u32, String> {
        validate_generation_nonce(generation_nonce)?;
        validate_timestamp(now_ms)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin project generation recovery: {error}"))?;
        let released = tx
            .execute(
                "UPDATE project_turn_leases SET released_at_ms=?2
                 WHERE generation_nonce=?1 AND released_at_ms IS NULL",
                params![generation_nonce, now_ms],
            )
            .map_err(|error| format!("release stopped project worker generation: {error}"))?;
        prune_released_leases(
            &tx,
            now_ms,
            RELEASED_LEASE_RETENTION_MS,
            MAX_RETAINED_RELEASED_LEASES,
        )?;
        tx.commit()
            .map_err(|error| format!("commit project generation recovery: {error}"))?;
        u32::try_from(released).map_err(|_| "released project lease count is out of range".into())
    }

    /// Release a terminal turn lease owned by this exact managed worker generation.
    ///
    /// The turn must have a terminal outcome, but its worker process may remain
    /// alive and serve later turns. If the process exits without a terminal
    /// outcome, use [`Self::release_generation_after_exit`] only after the exact
    /// process receipt confirms that generation has stopped.
    pub fn release_turn(
        &self,
        turn_id: &str,
        generation_nonce: &str,
        now_ms: i64,
    ) -> Result<ProjectLeaseRelease, String> {
        let turn_id = canonical_turn_id(turn_id)?;
        validate_generation_nonce(generation_nonce)?;
        validate_timestamp(now_ms)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin project turn release: {error}"))?;
        let prior: Option<(String, Option<i64>)> = tx
            .query_row(
                "SELECT generation_nonce, released_at_ms
                 FROM project_turn_leases WHERE turn_id=?1",
                [&turn_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("read project turn lease for release: {error}"))?;
        let outcome = match prior {
            None => ProjectLeaseRelease::UnknownTurn,
            Some((prior_nonce, _)) if prior_nonce != generation_nonce => {
                return Err("worker generation does not own this project turn lease".into());
            }
            Some((_, Some(_))) => ProjectLeaseRelease::AlreadyReleased,
            Some((_, None)) => {
                tx.execute(
                    "UPDATE project_turn_leases SET released_at_ms=?2
                     WHERE turn_id=?1 AND released_at_ms IS NULL",
                    params![&turn_id, now_ms],
                )
                .map_err(|error| format!("release project turn lease: {error}"))?;
                prune_released_leases(
                    &tx,
                    now_ms,
                    RELEASED_LEASE_RETENTION_MS,
                    MAX_RETAINED_RELEASED_LEASES,
                )?;
                ProjectLeaseRelease::Released
            }
        };
        tx.commit()
            .map_err(|error| format!("commit project turn release: {error}"))?;
        Ok(outcome)
    }

    fn connect(&self) -> Result<Connection, String> {
        if self
            .db_path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("project admission database is a symlink".into());
        }
        let mut create = OpenOptions::new();
        create.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            create.mode(0o600);
        }
        match create.open(&self.db_path) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("create Buzz project admission file: {error}")),
        }
        let metadata = self
            .db_path
            .symlink_metadata()
            .map_err(|error| format!("inspect Buzz project admission file: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err("project admission path is not a regular file".into());
        }
        let conn = Connection::open(&self.db_path)
            .map_err(|error| format!("open Buzz project admission journal: {error}"))?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(|error| format!("configure project admission journal: {error}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| format!("configure project admission journal: {error}"))?;
        Ok(conn)
    }
}

fn canonical_project_coordinate(coordinate: &str) -> Result<String, String> {
    validate_project_coordinate(coordinate)?;
    let mut parts = coordinate.splitn(3, ':');
    let kind = parts.next().unwrap_or_default();
    let owner = parts.next().unwrap_or_default().to_ascii_lowercase();
    let slug = parts.next().unwrap_or_default();
    Ok(format!("{kind}:{owner}:{slug}"))
}

fn validate_generation_nonce(nonce: &str) -> Result<(), String> {
    if nonce.len() != 32
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid worker generation nonce".into());
    }
    Ok(())
}

fn canonical_turn_id(turn_id: &str) -> Result<String, String> {
    validate_turn_id(turn_id)?;
    Uuid::parse_str(turn_id)
        .map(|uuid| uuid.hyphenated().to_string())
        .map_err(|_| "invalid managed turn ID".into())
}

fn validate_timestamp(now_ms: i64) -> Result<(), String> {
    if now_ms < 0 {
        return Err("project admission timestamp must be non-negative".into());
    }
    Ok(())
}

fn configured_limit(conn: &Connection, project_coordinate: &str) -> Result<Option<u32>, String> {
    let limit: Option<Option<i64>> = conn
        .query_row(
            "SELECT max_active FROM project_admission_policy WHERE project_coordinate=?1",
            [project_coordinate],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("read project turn limit: {error}"))?;
    match limit.flatten() {
        Some(value) => u32::try_from(value)
            .map(Some)
            .map_err(|_| "stored project turn limit is out of range".into()),
        None => Ok(None),
    }
}

fn active_count(conn: &Connection, project_coordinate: &str) -> Result<u32, String> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM project_turn_leases
             WHERE project_coordinate=?1 AND released_at_ms IS NULL",
            [project_coordinate],
            |row| row.get(0),
        )
        .map_err(|error| format!("count active project turn leases: {error}"))?;
    u32::try_from(count).map_err(|_| "active project turn count is out of range".into())
}

fn ensure_policy_capacity(
    conn: &Connection,
    project_coordinate: &str,
    maximum: usize,
) -> Result<(), String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM project_admission_policy WHERE project_coordinate=?1)",
            [project_coordinate],
            |row| row.get(0),
        )
        .map_err(|error| format!("check project admission policy: {error}"))?;
    if exists {
        return Ok(());
    }
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_admission_policy", [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("count project admission policies: {error}"))?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= maximum {
        return Err(format!(
            "project admission policy capacity reached (maximum {maximum})"
        ));
    }
    Ok(())
}

fn prune_released_leases(
    conn: &Connection,
    now_ms: i64,
    retention_ms: i64,
    maximum: usize,
) -> Result<(), String> {
    let cutoff_ms = now_ms.saturating_sub(retention_ms);
    conn.execute(
        "DELETE FROM project_turn_leases
         WHERE released_at_ms IS NOT NULL AND released_at_ms < ?1",
        [cutoff_ms],
    )
    .map_err(|error| format!("expire old released project leases: {error}"))?;
    let maximum = i64::try_from(maximum).unwrap_or(i64::MAX);
    conn.execute(
        "DELETE FROM project_turn_leases
         WHERE turn_id IN (
             SELECT turn_id FROM project_turn_leases
             WHERE released_at_ms IS NOT NULL
             ORDER BY released_at_ms DESC, turn_id DESC
             LIMIT -1 OFFSET ?1
         )",
        [maximum],
    )
    .map_err(|error| format!("bound released project lease history: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;

    const RELAY: &str = "https://relay.example/";
    const OWNER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PROJECT: &str =
        "30621:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:workspace";
    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn journal(temp: &TempDir) -> ProjectAdmissionJournal {
        ProjectAdmissionJournal::open_scoped(temp.path(), RELAY, OWNER).unwrap()
    }

    fn turn() -> String {
        Uuid::new_v4().to_string()
    }

    #[test]
    fn no_policy_does_not_create_a_lease() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        assert_eq!(
            journal.reserve_turn(PROJECT, &turn(), None, 1).unwrap(),
            ProjectReserveOutcome::NotLimited
        );
        assert_eq!(
            journal.status(PROJECT).unwrap(),
            ProjectAdmissionStatus {
                max_active: None,
                active: 0
            }
        );
    }

    #[test]
    fn finite_limit_requires_generation_and_status_tracks_leases() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        assert_eq!(journal.set_limit(PROJECT, 1, 1).unwrap(), 0);
        let id = turn();
        assert!(journal.reserve_turn(PROJECT, &id, None, 2).is_err());
        assert_eq!(
            journal.status(PROJECT).unwrap(),
            ProjectAdmissionStatus {
                max_active: Some(1),
                active: 0
            }
        );
        assert_eq!(
            journal.reserve_turn(PROJECT, &id, Some(NONCE), 3).unwrap(),
            ProjectReserveOutcome::Admitted {
                active: 1,
                limit: 1
            }
        );
        assert_eq!(
            journal.status(PROJECT).unwrap(),
            ProjectAdmissionStatus {
                max_active: Some(1),
                active: 1
            }
        );
        assert_eq!(
            journal.release_turn(&id, NONCE, 4).unwrap(),
            ProjectLeaseRelease::Released
        );
        assert_eq!(
            journal.status(PROJECT).unwrap(),
            ProjectAdmissionStatus {
                max_active: Some(1),
                active: 0
            }
        );
    }

    #[test]
    fn exact_generation_recovery_releases_its_leases_only() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        let another =
            "30621:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:other";
        journal.set_limit(PROJECT, 2, 1).unwrap();
        journal.set_limit(another, 2, 1).unwrap();
        journal
            .reserve_turn(PROJECT, &turn(), Some(NONCE), 2)
            .unwrap();
        journal
            .reserve_turn(PROJECT, &turn(), Some(NONCE), 3)
            .unwrap();
        journal
            .reserve_turn(another, &turn(), Some(NONCE), 4)
            .unwrap();
        journal
            .reserve_turn(
                another,
                &turn(),
                Some("fedcba9876543210fedcba9876543210"),
                5,
            )
            .unwrap();

        assert_eq!(journal.release_generation_after_exit(NONCE, 6).unwrap(), 3);
        assert_eq!(journal.release_generation_after_exit(NONCE, 7).unwrap(), 0);
        assert_eq!(
            journal.status(PROJECT).unwrap(),
            ProjectAdmissionStatus {
                max_active: Some(2),
                active: 0
            }
        );
        assert_eq!(
            journal.status(another).unwrap(),
            ProjectAdmissionStatus {
                max_active: Some(2),
                active: 1
            }
        );
        assert!(journal.release_generation_after_exit("invalid", 8).is_err());
    }

    #[test]
    fn limit_reserves_replays_and_releases_only_the_exact_generation() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        assert_eq!(journal.set_limit(PROJECT, 1, 1).unwrap(), 0);
        let id = turn();
        assert_eq!(
            journal.reserve_turn(PROJECT, &id, Some(NONCE), 2).unwrap(),
            ProjectReserveOutcome::Admitted {
                active: 1,
                limit: 1
            }
        );
        assert_eq!(
            journal.reserve_turn(PROJECT, &id, Some(NONCE), 3).unwrap(),
            ProjectReserveOutcome::AlreadyReserved {
                active: 1,
                limit: Some(1)
            }
        );
        assert_eq!(
            journal
                .reserve_turn(PROJECT, &id.to_ascii_uppercase(), Some(NONCE), 3)
                .unwrap(),
            ProjectReserveOutcome::AlreadyReserved {
                active: 1,
                limit: Some(1)
            }
        );
        assert!(journal
            .release_turn(&id, "ffffffffffffffffffffffffffffffff", 4)
            .is_err());
        assert_eq!(
            journal.release_turn(&id, NONCE, 4).unwrap(),
            ProjectLeaseRelease::Released
        );
        assert_eq!(
            journal.release_turn(&id, NONCE, 5).unwrap(),
            ProjectLeaseRelease::AlreadyReleased
        );
        assert_eq!(journal.set_limit(PROJECT, 1, 6).unwrap(), 0);
        assert!(journal.reserve_turn(PROJECT, &id, Some(NONCE), 7).is_err());
    }

    #[test]
    fn limit_isolated_by_project_and_survives_reopen() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        journal.set_limit(PROJECT, 1, 1).unwrap();
        let id = turn();
        journal.reserve_turn(PROJECT, &id, Some(NONCE), 2).unwrap();
        let reopened = ProjectAdmissionJournal::open_scoped(
            temp.path(),
            "wss://relay.example",
            &format!("{OWNER}").to_ascii_uppercase(),
        )
        .unwrap();
        assert!(matches!(
            reopened
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 3)
                .unwrap(),
            ProjectReserveOutcome::AtCapacity {
                active: 1,
                limit: 1
            }
        ));
        let another =
            "30621:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:other";
        assert_eq!(
            reopened
                .reserve_turn(another, &turn(), Some(NONCE), 4)
                .unwrap(),
            ProjectReserveOutcome::NotLimited
        );
    }

    #[test]
    fn concurrent_reservations_cannot_overbook() {
        let temp = TempDir::new().unwrap();
        let journal = Arc::new(journal(&temp));
        journal.set_limit(PROJECT, 2, 1).unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let workers = (0..8)
            .map(|_| {
                let journal = Arc::clone(&journal);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let id = turn();
                    barrier.wait();
                    journal.reserve_turn(PROJECT, &id, Some(NONCE), 2).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ProjectReserveOutcome::Admitted { .. }))
                .count(),
            2
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ProjectReserveOutcome::AtCapacity { .. }))
                .count(),
            6
        );
    }

    #[test]
    fn admission_is_isolated_by_owner_and_relay() {
        let temp = TempDir::new().unwrap();
        let first = journal(&temp);
        let second_owner = ProjectAdmissionJournal::open_scoped(
            temp.path(),
            RELAY,
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        )
        .unwrap();
        let second_relay =
            ProjectAdmissionJournal::open_scoped(temp.path(), "wss://other-relay.example", OWNER)
                .unwrap();
        first.set_limit(PROJECT, 1, 1).unwrap();
        second_owner.set_limit(PROJECT, 1, 1).unwrap();
        second_relay.set_limit(PROJECT, 1, 1).unwrap();

        first
            .reserve_turn(PROJECT, &turn(), Some(NONCE), 2)
            .unwrap();
        assert!(matches!(
            first
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 3)
                .unwrap(),
            ProjectReserveOutcome::AtCapacity {
                active: 1,
                limit: 1
            }
        ));
        assert!(matches!(
            second_owner
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 3)
                .unwrap(),
            ProjectReserveOutcome::Admitted {
                active: 1,
                limit: 1
            }
        ));
        assert!(matches!(
            second_relay
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 3)
                .unwrap(),
            ProjectReserveOutcome::Admitted {
                active: 1,
                limit: 1
            }
        ));
    }

    #[test]
    fn clearing_policy_preserves_existing_lease_for_replay_and_release() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        journal.set_limit(PROJECT, 1, 1).unwrap();
        let id = turn();
        journal.reserve_turn(PROJECT, &id, Some(NONCE), 2).unwrap();
        journal.clear_limit(PROJECT, 3).unwrap();
        assert_eq!(
            journal.reserve_turn(PROJECT, &id, Some(NONCE), 4).unwrap(),
            ProjectReserveOutcome::AlreadyReserved {
                active: 1,
                limit: None
            }
        );
        assert_eq!(
            journal
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 5)
                .unwrap(),
            ProjectReserveOutcome::NotLimited
        );
        assert_eq!(
            journal.release_turn(&id, NONCE, 6).unwrap(),
            ProjectLeaseRelease::Released
        );
    }

    #[test]
    fn terminal_turn_frees_project_slot_while_worker_generation_stays_reusable() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        journal.set_limit(PROJECT, 1, 1).unwrap();

        let first_turn = turn();
        journal
            .reserve_turn(PROJECT, &first_turn, Some(NONCE), 2)
            .unwrap();
        assert_eq!(
            journal.release_turn(&first_turn, NONCE, 3).unwrap(),
            ProjectLeaseRelease::Released
        );

        assert_eq!(
            journal
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 4)
                .unwrap(),
            ProjectReserveOutcome::Admitted {
                active: 1,
                limit: 1
            }
        );
    }

    #[test]
    fn lowering_limit_keeps_existing_leases_and_blocks_new_work() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        journal.set_limit(PROJECT, 2, 1).unwrap();
        journal
            .reserve_turn(PROJECT, &turn(), Some(NONCE), 2)
            .unwrap();
        journal
            .reserve_turn(PROJECT, &turn(), Some(NONCE), 3)
            .unwrap();
        assert_eq!(journal.set_limit(PROJECT, 1, 4).unwrap(), 2);
        assert!(matches!(
            journal
                .reserve_turn(PROJECT, &turn(), Some(NONCE), 5)
                .unwrap(),
            ProjectReserveOutcome::AtCapacity {
                active: 2,
                limit: 1
            }
        ));
    }

    #[test]
    fn validation_rejects_zero_limits_bad_ids_and_bad_nonces() {
        let temp = TempDir::new().unwrap();
        let journal = journal(&temp);
        assert!(journal.set_limit(PROJECT, 0, 1).is_err());
        assert!(journal.set_limit(PROJECT, 1, -1).is_err());
        assert!(journal
            .reserve_turn(PROJECT, "not-a-uuid", Some(NONCE), 1)
            .is_err());
        assert!(journal
            .reserve_turn(PROJECT, &turn(), Some("NOT-A-NONCE"), 1)
            .is_err());
        assert!(journal
            .reserve_turn("30621:not-owner:bad", &turn(), Some(NONCE), 1)
            .is_err());
    }

    #[test]
    fn policy_capacity_and_released_lease_history_are_bounded() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE project_admission_policy(project_coordinate TEXT PRIMARY KEY);
             CREATE TABLE project_turn_leases(
                turn_id TEXT PRIMARY KEY,
                project_coordinate TEXT NOT NULL,
                generation_nonce TEXT NOT NULL,
                reserved_at_ms INTEGER NOT NULL,
                released_at_ms INTEGER
             );",
        )
        .unwrap();
        conn.execute("INSERT INTO project_admission_policy VALUES ('a')", [])
            .unwrap();
        conn.execute("INSERT INTO project_admission_policy VALUES ('b')", [])
            .unwrap();
        assert!(ensure_policy_capacity(&conn, "a", 2).is_ok());
        assert!(ensure_policy_capacity(&conn, "c", 2).is_err());

        for (id, released_at_ms) in [
            ("old", 1),
            ("recent-1", 60_000),
            ("recent-2", 70_000),
            ("recent-3", 80_000),
        ] {
            conn.execute(
                "INSERT INTO project_turn_leases
                 VALUES (?1, 'a', '0123456789abcdef0123456789abcdef', 0, ?2)",
                params![id, released_at_ms],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO project_turn_leases
             VALUES ('active', 'a', '0123456789abcdef0123456789abcdef', 0, NULL)",
            [],
        )
        .unwrap();
        prune_released_leases(&conn, 100_000, 50_000, 2).unwrap();
        let retained: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_turn_leases WHERE released_at_ms IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_turn_leases WHERE released_at_ms IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, 2);
        assert_eq!(active, 1);
    }
}
