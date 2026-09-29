//! Durable goal and task records for Buzz's local coordinator.
//!
//! A goal run is deliberately separate from an ACP turn. A turn proves only
//! that a worker transport ran; a goal is complete only when its task evidence
//! has been accepted by the coordinator.

use std::collections::BTreeSet;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{now_ms, validate_event_id, RunJournal};

const MAX_GOAL_RUNS: usize = 4_096;
const MAX_GOAL_TASKS: usize = 6;
const MAX_GOAL_TEXT_BYTES: usize = 24 * 1024;
const MAX_TASK_TEXT_BYTES: usize = 16 * 1024;
const MAX_EVIDENCE_BYTES: usize = 8 * 1024;
const MAX_EVIDENCE_PER_TASK: usize = 32;

/// Stable lifecycle state for a locally coordinated goal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalRunState {
    Draft,
    Ready,
    Running,
    NeedsGuidance,
    Completed,
    Cancelled,
}

impl GoalRunState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::NeedsGuidance => "needs_guidance",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "draft" => Ok(Self::Draft),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "needs_guidance" => Ok(Self::NeedsGuidance),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err("stored goal run has an invalid state".into()),
        }
    }
}

/// State controlled by the coordinator, never inferred from a worker reply.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalTaskState {
    Planned,
    Ready,
    Running,
    NeedsEvidence,
    Accepted,
    Blocked,
    Cancelled,
}

impl GoalTaskState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::NeedsEvidence => "needs_evidence",
            Self::Accepted => "accepted",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "planned" => Ok(Self::Planned),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "needs_evidence" => Ok(Self::NeedsEvidence),
            "accepted" => Ok(Self::Accepted),
            "blocked" => Ok(Self::Blocked),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err("stored goal task has an invalid state".into()),
        }
    }
}

/// A bounded, validated task supplied by a planner or an explicit user plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalTaskSpec {
    pub title: String,
    pub instructions: String,
    pub acceptance_criteria: String,
    #[serde(default)]
    pub depends_on: Vec<usize>,
}

/// The data needed to create a local goal run from one already-published chat message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalRunSpec {
    pub channel_id: String,
    pub session_scope: String,
    pub source_event_id: String,
    pub thread_root_event_id: Option<String>,
    pub goal: String,
    pub max_parallel: u8,
    pub tasks: Vec<GoalTaskSpec>,
}

/// Durable task evidence. `reference` may be an artifact ID, a local path, or
/// a short validation receipt. It is not treated as trusted execution output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoalTaskEvidence {
    pub evidence_id: String,
    pub kind: String,
    pub reference: String,
    pub created_at_ms: i64,
}

/// Durable task state, dependencies, and accepted evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoalTaskRecord {
    pub task_id: String,
    pub ordinal: usize,
    pub title: String,
    pub instructions: String,
    pub acceptance_criteria: String,
    pub depends_on: Vec<String>,
    pub state: GoalTaskState,
    pub generation: u32,
    pub assigned_agent_pubkey: Option<String>,
    pub attempt_turn_ids: Vec<String>,
    pub evidence: Vec<GoalTaskEvidence>,
}

/// A source-linked local goal with a bounded task graph.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoalRunRecord {
    pub goal_run_id: String,
    pub channel_id: String,
    pub session_scope: String,
    pub source_event_id: String,
    pub thread_root_event_id: Option<String>,
    pub goal: String,
    pub state: GoalRunState,
    pub scope_version: u32,
    pub max_parallel: u8,
    pub tasks: Vec<GoalTaskRecord>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub(crate) fn ensure_goal_runs(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS goal_runs (
            goal_run_id TEXT PRIMARY KEY,
            channel_id TEXT NOT NULL,
            session_scope TEXT NOT NULL,
            source_event_id TEXT NOT NULL,
            thread_root_event_id TEXT,
            goal TEXT NOT NULL,
            state TEXT NOT NULL,
            scope_version INTEGER NOT NULL,
            max_parallel INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            UNIQUE(channel_id, session_scope, source_event_id)
        );
        CREATE TABLE IF NOT EXISTS goal_tasks (
            goal_run_id TEXT NOT NULL REFERENCES goal_runs(goal_run_id),
            task_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            title TEXT NOT NULL,
            instructions TEXT NOT NULL,
            acceptance_criteria TEXT NOT NULL,
            state TEXT NOT NULL,
            generation INTEGER NOT NULL,
            PRIMARY KEY(goal_run_id, task_id),
            UNIQUE(goal_run_id, ordinal)
        );
        CREATE TABLE IF NOT EXISTS goal_task_dependencies (
            goal_run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            dependency_task_id TEXT NOT NULL,
            PRIMARY KEY(goal_run_id, task_id, dependency_task_id),
            FOREIGN KEY(goal_run_id, task_id) REFERENCES goal_tasks(goal_run_id, task_id),
            FOREIGN KEY(goal_run_id, dependency_task_id) REFERENCES goal_tasks(goal_run_id, task_id)
        );
        CREATE TABLE IF NOT EXISTS goal_task_evidence (
            goal_run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            evidence_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            reference TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY(goal_run_id, task_id, evidence_id),
            FOREIGN KEY(goal_run_id, task_id) REFERENCES goal_tasks(goal_run_id, task_id)
        );
        CREATE TABLE IF NOT EXISTS goal_task_sources (
            goal_run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            generation INTEGER NOT NULL,
            source_event_id TEXT NOT NULL UNIQUE,
            assigned_agent_pubkey TEXT NOT NULL,
            PRIMARY KEY(goal_run_id, task_id, source_event_id),
            FOREIGN KEY(goal_run_id, task_id) REFERENCES goal_tasks(goal_run_id, task_id)
        );
        CREATE TABLE IF NOT EXISTS goal_task_attempts (
            goal_run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            generation INTEGER NOT NULL,
            PRIMARY KEY(goal_run_id, task_id, turn_id),
            FOREIGN KEY(goal_run_id, task_id) REFERENCES goal_tasks(goal_run_id, task_id)
        );
        CREATE INDEX IF NOT EXISTS goal_runs_by_updated
            ON goal_runs(channel_id, updated_at_ms DESC, goal_run_id DESC);
        CREATE INDEX IF NOT EXISTS goal_task_evidence_by_task
            ON goal_task_evidence(goal_run_id, task_id, created_at_ms);
        CREATE INDEX IF NOT EXISTS goal_task_sources_by_event
            ON goal_task_sources(source_event_id);",
    )
    .map_err(|error| format!("initialize goal run journal: {error}"))?;
    let has_assignee = conn
        .prepare("PRAGMA table_info(goal_task_sources)")
        .map_err(|error| format!("inspect goal task source schema: {error}"))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("read goal task source schema: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("decode goal task source schema: {error}"))?
        .iter()
        .any(|column| column == "assigned_agent_pubkey");
    if !has_assignee {
        conn.execute(
            "ALTER TABLE goal_task_sources ADD COLUMN assigned_agent_pubkey TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|error| format!("upgrade goal task source schema: {error}"))?;
    }
    conn.execute(
        "INSERT OR IGNORE INTO managed_run_journal_migrations(version) VALUES (3)",
        [],
    )
    .map_err(|error| format!("record goal-run migration: {error}"))?;
    Ok(())
}

impl RunJournal {
    /// Create a local goal run exactly once for a published source message.
    /// Repeating the same call returns the existing record instead of making a
    /// second plan or duplicating downstream worker dispatch.
    pub fn create_goal_run(&self, spec: GoalRunSpec) -> Result<GoalRunRecord, String> {
        validate_goal_run_spec(&spec)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin goal run creation: {error}"))?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT goal_run_id FROM goal_runs
                 WHERE channel_id=?1 AND session_scope=?2 AND source_event_id=?3",
                params![&spec.channel_id, &spec.session_scope, &spec.source_event_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("read existing goal run: {error}"))?;
        if let Some(goal_run_id) = existing {
            tx.commit()
                .map_err(|error| format!("finish idempotent goal run creation: {error}"))?;
            return self
                .goal_run(&goal_run_id)?
                .ok_or_else(|| "existing goal run disappeared during creation".to_string());
        }
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM goal_runs", [], |row| row.get(0))
            .map_err(|error| format!("count local goal runs: {error}"))?;
        if count >= MAX_GOAL_RUNS as i64 {
            return Err("local goal-run journal reached its safety limit".into());
        }
        let now = now_ms();
        let goal_run_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO goal_runs(
                goal_run_id, channel_id, session_scope, source_event_id,
                thread_root_event_id, goal, state, scope_version, max_parallel,
                created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', 1, ?7, ?8, ?8)",
            params![
                &goal_run_id,
                &spec.channel_id,
                &spec.session_scope,
                spec.source_event_id.to_ascii_lowercase(),
                spec.thread_root_event_id
                    .as_deref()
                    .map(str::to_ascii_lowercase),
                &spec.goal,
                i64::from(spec.max_parallel),
                now,
            ],
        )
        .map_err(|error| format!("store goal run: {error}"))?;
        let task_ids = (0..spec.tasks.len())
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>();
        for (ordinal, task) in spec.tasks.iter().enumerate() {
            let state = if task.depends_on.is_empty() {
                "ready"
            } else {
                "planned"
            };
            tx.execute(
                "INSERT INTO goal_tasks(
                    goal_run_id, task_id, ordinal, title, instructions,
                    acceptance_criteria, state, generation
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                params![
                    &goal_run_id,
                    &task_ids[ordinal],
                    ordinal as i64,
                    &task.title,
                    &task.instructions,
                    &task.acceptance_criteria,
                    state,
                ],
            )
            .map_err(|error| format!("store goal task: {error}"))?;
            for dependency in &task.depends_on {
                tx.execute(
                    "INSERT INTO goal_task_dependencies(
                        goal_run_id, task_id, dependency_task_id
                    ) VALUES (?1, ?2, ?3)",
                    params![&goal_run_id, &task_ids[ordinal], &task_ids[*dependency]],
                )
                .map_err(|error| format!("store goal task dependency: {error}"))?;
            }
        }
        tx.commit()
            .map_err(|error| format!("commit goal run creation: {error}"))?;
        self.goal_run(&goal_run_id)?
            .ok_or_else(|| "new goal run was not readable".to_string())
    }

    /// Persist one planner-proposed task DAG after the root planning task.
    /// The planner remains evidence-gated; this only turns a validated plan
    /// into durable downstream work and may be done once per goal run.
    pub fn append_goal_plan(
        &self,
        goal_run_id: &str,
        planner_task_id: &str,
        generation: u32,
        reporter_pubkey: &str,
        tasks: &[GoalTaskSpec],
    ) -> Result<GoalRunRecord, String> {
        validate_goal_task_specs(tasks, MAX_GOAL_TASKS - 1)?;
        let goal_run_id = canonical_uuid(goal_run_id, "goal run ID")?;
        let planner_task_id = canonical_uuid(planner_task_id, "goal task ID")?;
        let reporter_pubkey = normalize_pubkey(reporter_pubkey)?;
        if generation == 0 {
            return Err("goal task generation must be positive".into());
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin goal plan append: {error}"))?;
        let current = ensure_task_generation(&tx, &goal_run_id, &planner_task_id, generation)?;
        if !matches!(
            current,
            GoalTaskState::Running | GoalTaskState::NeedsEvidence
        ) {
            return Err("planner task is not accepting a plan".into());
        }
        let assignee: Option<String> = tx
            .query_row(
                "SELECT assigned_agent_pubkey FROM goal_task_sources
                 WHERE goal_run_id=?1 AND task_id=?2 AND generation=?3",
                params![&goal_run_id, &planner_task_id, generation],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("read planner task assignee: {error}"))?;
        if assignee.as_deref() != Some(reporter_pubkey.as_str()) {
            return Err("goal plan sender is not the assigned managed agent".into());
        }
        let scope_version: u32 = tx
            .query_row(
                "SELECT scope_version FROM goal_runs WHERE goal_run_id=?1",
                [&goal_run_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("read goal plan version: {error}"))?;
        if scope_version != 1 {
            return Err("goal plan was already applied".into());
        }
        let existing: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM goal_tasks WHERE goal_run_id=?1",
                [&goal_run_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("count goal tasks before plan append: {error}"))?;
        if existing != 1 {
            return Err("goal plan can only extend its root planning task".into());
        }
        let task_ids = (0..tasks.len())
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>();
        for (index, task) in tasks.iter().enumerate() {
            tx.execute(
                "INSERT INTO goal_tasks(
                    goal_run_id, task_id, ordinal, title, instructions,
                    acceptance_criteria, state, generation
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'planned', 1)",
                params![
                    &goal_run_id,
                    &task_ids[index],
                    existing + index as i64,
                    &task.title,
                    &task.instructions,
                    &task.acceptance_criteria,
                ],
            )
            .map_err(|error| format!("store planned goal task: {error}"))?;
            tx.execute(
                "INSERT INTO goal_task_dependencies(goal_run_id, task_id, dependency_task_id)
                 VALUES (?1, ?2, ?3)",
                params![&goal_run_id, &task_ids[index], &planner_task_id],
            )
            .map_err(|error| format!("store planner dependency: {error}"))?;
            for dependency in &task.depends_on {
                tx.execute(
                    "INSERT INTO goal_task_dependencies(goal_run_id, task_id, dependency_task_id)
                     VALUES (?1, ?2, ?3)",
                    params![&goal_run_id, &task_ids[index], &task_ids[*dependency]],
                )
                .map_err(|error| format!("store planned goal task dependency: {error}"))?;
            }
        }
        tx.execute(
            "UPDATE goal_runs SET scope_version=2, updated_at_ms=?2 WHERE goal_run_id=?1",
            params![&goal_run_id, now_ms()],
        )
        .map_err(|error| format!("mark goal plan applied: {error}"))?;
        tx.commit()
            .map_err(|error| format!("commit goal plan append: {error}"))?;
        self.goal_run(&goal_run_id)?
            .ok_or_else(|| "goal run disappeared after planner handoff".to_string())
    }

    /// Read one local goal by its stable ID.
    pub fn goal_run(&self, goal_run_id: &str) -> Result<Option<GoalRunRecord>, String> {
        let goal_run_id = canonical_uuid(goal_run_id, "goal run ID")?;
        let conn = self.connect()?;
        load_goal_run(&conn, &goal_run_id)
    }

    /// Read recent goals for one channel. This local result is not relay proof.
    pub fn recent_goal_runs(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<GoalRunRecord>, String> {
        let channel_id = canonical_channel_id(channel_id)?;
        if !(1..=50).contains(&limit) {
            return Err("goal run history limit must be between 1 and 50".into());
        }
        let conn = self.connect()?;
        let mut statement = conn
            .prepare(
                "SELECT goal_run_id FROM goal_runs WHERE channel_id=?1
                 ORDER BY updated_at_ms DESC, goal_run_id DESC LIMIT ?2",
            )
            .map_err(|error| format!("prepare goal run history: {error}"))?;
        let ids = statement
            .query_map(params![channel_id, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("read goal run history: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode goal run history: {error}"))?;
        ids.into_iter()
            .map(|id| {
                load_goal_run(&conn, &id)?
                    .ok_or_else(|| "goal run disappeared during history read".to_string())
            })
            .collect()
    }

    /// Append inspectable evidence for the exact current task generation.
    pub fn append_goal_task_evidence(
        &self,
        goal_run_id: &str,
        task_id: &str,
        generation: u32,
        kind: &str,
        reference: &str,
    ) -> Result<GoalTaskEvidence, String> {
        let goal_run_id = canonical_uuid(goal_run_id, "goal run ID")?;
        let task_id = canonical_uuid(task_id, "goal task ID")?;
        validate_evidence(kind, reference)?;
        if generation == 0 {
            return Err("goal task generation must be positive".into());
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin goal evidence append: {error}"))?;
        ensure_task_generation(&tx, &goal_run_id, &task_id, generation)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM goal_task_evidence
                 WHERE goal_run_id=?1 AND task_id=?2",
                params![&goal_run_id, &task_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("count task evidence: {error}"))?;
        if count >= MAX_EVIDENCE_PER_TASK as i64 {
            return Err("goal task reached its evidence safety limit".into());
        }
        let evidence = GoalTaskEvidence {
            evidence_id: Uuid::new_v4().to_string(),
            kind: kind.to_owned(),
            reference: reference.to_owned(),
            created_at_ms: now_ms(),
        };
        tx.execute(
            "INSERT INTO goal_task_evidence(
                goal_run_id, task_id, evidence_id, kind, reference, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &goal_run_id,
                &task_id,
                &evidence.evidence_id,
                &evidence.kind,
                &evidence.reference,
                evidence.created_at_ms,
            ],
        )
        .map_err(|error| format!("store goal task evidence: {error}"))?;
        tx.execute(
            "UPDATE goal_runs SET updated_at_ms=?2 WHERE goal_run_id=?1",
            params![&goal_run_id, evidence.created_at_ms],
        )
        .map_err(|error| format!("touch goal run after evidence: {error}"))?;
        tx.commit()
            .map_err(|error| format!("commit goal evidence append: {error}"))?;
        Ok(evidence)
    }

    /// Bind an accepted task-assignment message to an exact task generation.
    /// Later ACP starts can join their trigger event to this durable task.
    pub fn bind_goal_task_assignment_source(
        &self,
        goal_run_id: &str,
        task_id: &str,
        generation: u32,
        source_event_id: &str,
        assigned_agent_pubkey: &str,
    ) -> Result<(), String> {
        let goal_run_id = canonical_uuid(goal_run_id, "goal run ID")?;
        let task_id = canonical_uuid(task_id, "goal task ID")?;
        validate_event_id(source_event_id)?;
        let assigned_agent_pubkey = normalize_pubkey(assigned_agent_pubkey)?;
        if generation == 0 {
            return Err("goal task generation must be positive".into());
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin goal task source bind: {error}"))?;
        ensure_task_generation(&tx, &goal_run_id, &task_id, generation)?;
        tx.execute(
            "INSERT OR IGNORE INTO goal_task_sources(
                goal_run_id, task_id, generation, source_event_id, assigned_agent_pubkey
            ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &goal_run_id,
                &task_id,
                generation,
                source_event_id.to_ascii_lowercase(),
                assigned_agent_pubkey,
            ],
        )
        .map_err(|error| format!("store goal task assignment source: {error}"))?;
        tx.commit()
            .map_err(|error| format!("commit goal task source bind: {error}"))
    }

    /// Accept a bounded structured report from the exact assigned managed
    /// agent. Reports add evidence and may request verification or guidance;
    /// they cannot mark a task accepted.
    #[allow(clippy::too_many_arguments)] // journal API mirrors the signed report envelope
    pub fn ingest_goal_task_report(
        &self,
        goal_run_id: &str,
        task_id: &str,
        generation: u32,
        reporter_pubkey: &str,
        report_event_id: &str,
        state: GoalTaskState,
        evidence: &[String],
    ) -> Result<GoalRunRecord, String> {
        let goal_run_id = canonical_uuid(goal_run_id, "goal run ID")?;
        let task_id = canonical_uuid(task_id, "goal task ID")?;
        let reporter_pubkey = normalize_pubkey(reporter_pubkey)?;
        validate_event_id(report_event_id)?;
        if generation == 0
            || !matches!(state, GoalTaskState::NeedsEvidence | GoalTaskState::Blocked)
        {
            return Err("goal report state is invalid".into());
        }
        if evidence.len() > 8 || (state == GoalTaskState::NeedsEvidence && evidence.is_empty()) {
            return Err("goal report needs between 1 and 8 evidence references".into());
        }
        for reference in evidence {
            validate_evidence("message", reference)?;
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin goal report ingestion: {error}"))?;
        let current = ensure_task_generation(&tx, &goal_run_id, &task_id, generation)?;
        if current != GoalTaskState::Running {
            return Err("goal task is not running".into());
        }
        let assignee: Option<String> = tx
            .query_row(
                "SELECT assigned_agent_pubkey FROM goal_task_sources
                 WHERE goal_run_id=?1 AND task_id=?2 AND generation=?3",
                params![&goal_run_id, &task_id, generation],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("read goal task assignee: {error}"))?;
        if assignee.as_deref() != Some(reporter_pubkey.as_str()) {
            return Err("goal report sender is not the assigned managed agent".into());
        }
        let existing: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM goal_task_evidence
                 WHERE goal_run_id=?1 AND task_id=?2 AND reference=?3)",
                params![&goal_run_id, &task_id, report_event_id.to_ascii_lowercase()],
                |row| row.get(0),
            )
            .map_err(|error| format!("check duplicate goal report: {error}"))?;
        if !existing {
            let now = now_ms();
            for reference in evidence {
                tx.execute(
                    "INSERT INTO goal_task_evidence(
                        goal_run_id, task_id, evidence_id, kind, reference, created_at_ms
                    ) VALUES (?1, ?2, ?3, 'message', ?4, ?5)",
                    params![
                        &goal_run_id,
                        &task_id,
                        Uuid::new_v4().to_string(),
                        reference,
                        now,
                    ],
                )
                .map_err(|error| format!("store goal report evidence: {error}"))?;
            }
            tx.execute(
                "INSERT INTO goal_task_evidence(
                    goal_run_id, task_id, evidence_id, kind, reference, created_at_ms
                ) VALUES (?1, ?2, ?3, 'message', ?4, ?5)",
                params![
                    &goal_run_id,
                    &task_id,
                    Uuid::new_v4().to_string(),
                    report_event_id.to_ascii_lowercase(),
                    now,
                ],
            )
            .map_err(|error| format!("store goal report receipt: {error}"))?;
        }
        tx.execute(
            "UPDATE goal_tasks SET state=?3 WHERE goal_run_id=?1 AND task_id=?2",
            params![&goal_run_id, &task_id, state.as_str()],
        )
        .map_err(|error| format!("update task from goal report: {error}"))?;
        refresh_goal_state(&tx, &goal_run_id)?;
        tx.commit()
            .map_err(|error| format!("commit goal report ingestion: {error}"))?;
        self.goal_run(&goal_run_id)?
            .ok_or_else(|| "goal run disappeared after report ingestion".to_string())
    }

    /// Move one task through the coordinator state machine.
    pub fn transition_goal_task(
        &self,
        goal_run_id: &str,
        task_id: &str,
        generation: u32,
        next: GoalTaskState,
    ) -> Result<GoalRunRecord, String> {
        let goal_run_id = canonical_uuid(goal_run_id, "goal run ID")?;
        let task_id = canonical_uuid(task_id, "goal task ID")?;
        if generation == 0 {
            return Err("goal task generation must be positive".into());
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin goal task transition: {error}"))?;
        let current = ensure_task_generation(&tx, &goal_run_id, &task_id, generation)?;
        validate_transition(&tx, &goal_run_id, &task_id, &current, &next)?;
        tx.execute(
            "UPDATE goal_tasks SET state=?3 WHERE goal_run_id=?1 AND task_id=?2",
            params![&goal_run_id, &task_id, next.as_str()],
        )
        .map_err(|error| format!("update goal task state: {error}"))?;
        release_ready_dependents(&tx, &goal_run_id)?;
        refresh_goal_state(&tx, &goal_run_id)?;
        tx.commit()
            .map_err(|error| format!("commit goal task transition: {error}"))?;
        self.goal_run(&goal_run_id)?
            .ok_or_else(|| "goal run disappeared after task transition".to_string())
    }
}

/// Join an ACP start to task scope when its exact trigger was a bound task
/// assignment message. This records transport provenance only, never success.
pub(crate) fn link_goal_task_attempts_for_turn(
    tx: &Transaction<'_>,
    turn_id: &str,
    trigger_event_ids: &[String],
) -> Result<(), String> {
    for source_event_id in trigger_event_ids {
        let source: Option<(String, String, u32)> = tx
            .query_row(
                "SELECT goal_run_id, task_id, generation FROM goal_task_sources
                 WHERE source_event_id=?1",
                [source_event_id.to_ascii_lowercase()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("read goal task source binding: {error}"))?;
        if let Some((goal_run_id, task_id, generation)) = source {
            tx.execute(
                "INSERT OR IGNORE INTO goal_task_attempts(
                    goal_run_id, task_id, turn_id, generation
                ) VALUES (?1, ?2, ?3, ?4)",
                params![goal_run_id, task_id, turn_id, generation],
            )
            .map_err(|error| format!("link goal task ACP attempt: {error}"))?;
        }
    }
    Ok(())
}

fn validate_goal_run_spec(spec: &GoalRunSpec) -> Result<(), String> {
    canonical_channel_id(&spec.channel_id)?;
    if !matches!(spec.session_scope.as_str(), "conversation" | "thread") {
        return Err("goal run session scope must be conversation or thread".into());
    }
    validate_event_id(&spec.source_event_id)?;
    match (
        spec.session_scope.as_str(),
        spec.thread_root_event_id.as_deref(),
    ) {
        ("thread", Some(root)) => validate_event_id(root)?,
        ("thread", None) => return Err("thread goal run needs a root event ID".into()),
        ("conversation", None) => {}
        ("conversation", Some(_)) => {
            return Err("conversation goal run cannot have a root event ID".into())
        }
        _ => unreachable!(),
    }
    validate_text(&spec.goal, MAX_GOAL_TEXT_BYTES, "goal")?;
    if !(1..=3).contains(&spec.max_parallel) {
        return Err("goal max parallelism must be between 1 and 3".into());
    }
    validate_goal_task_specs(&spec.tasks, MAX_GOAL_TASKS)
}

fn validate_goal_task_specs(tasks: &[GoalTaskSpec], max_tasks: usize) -> Result<(), String> {
    if tasks.is_empty() || tasks.len() > max_tasks {
        return Err(format!(
            "goal plans must contain between 1 and {max_tasks} tasks"
        ));
    }
    let mut titles = BTreeSet::new();
    for (index, task) in tasks.iter().enumerate() {
        validate_text(&task.title, 512, "goal task title")?;
        validate_text(
            &task.instructions,
            MAX_TASK_TEXT_BYTES,
            "goal task instructions",
        )?;
        validate_text(
            &task.acceptance_criteria,
            MAX_TASK_TEXT_BYTES,
            "goal task acceptance criteria",
        )?;
        if !titles.insert(task.title.trim().to_ascii_lowercase()) {
            return Err("goal task titles must be distinct".into());
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &task.depends_on {
            if *dependency >= tasks.len() || *dependency == index {
                return Err("goal task dependency is out of range".into());
            }
            if !dependencies.insert(*dependency) {
                return Err("goal task dependencies must be distinct".into());
            }
        }
    }
    validate_acyclic(tasks)
}

fn validate_acyclic(tasks: &[GoalTaskSpec]) -> Result<(), String> {
    fn visit(
        index: usize,
        tasks: &[GoalTaskSpec],
        visiting: &mut BTreeSet<usize>,
        visited: &mut BTreeSet<usize>,
    ) -> Result<(), String> {
        if visited.contains(&index) {
            return Ok(());
        }
        if !visiting.insert(index) {
            return Err("goal task dependencies contain a cycle".into());
        }
        for dependency in &tasks[index].depends_on {
            visit(*dependency, tasks, visiting, visited)?;
        }
        visiting.remove(&index);
        visited.insert(index);
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for index in 0..tasks.len() {
        visit(index, tasks, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(format!("invalid {label}"));
    }
    Ok(())
}

fn validate_evidence(kind: &str, reference: &str) -> Result<(), String> {
    if !matches!(kind, "artifact" | "validation" | "message" | "blocker") {
        return Err("goal evidence kind is invalid".into());
    }
    validate_text(reference, MAX_EVIDENCE_BYTES, "goal evidence reference")
}

fn canonical_channel_id(value: &str) -> Result<String, String> {
    Uuid::parse_str(value)
        .map_err(|_| "invalid goal run channel ID".to_string())
        .map(|id| id.to_string())
}

fn canonical_uuid(value: &str, label: &str) -> Result<String, String> {
    Uuid::parse_str(value)
        .map_err(|_| format!("invalid {label}"))
        .map(|id| id.to_string())
}

fn normalize_pubkey(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid managed agent pubkey".into());
    }
    Ok(value)
}

fn ensure_task_generation(
    tx: &Transaction<'_>,
    goal_run_id: &str,
    task_id: &str,
    generation: u32,
) -> Result<GoalTaskState, String> {
    let row: Option<(String, u32)> = tx
        .query_row(
            "SELECT state, generation FROM goal_tasks WHERE goal_run_id=?1 AND task_id=?2",
            params![goal_run_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("read goal task generation: {error}"))?;
    let Some((state, stored_generation)) = row else {
        return Err("goal task was not found".into());
    };
    if stored_generation != generation {
        return Err("goal task generation is stale".into());
    }
    GoalTaskState::parse(&state)
}

fn validate_transition(
    tx: &Transaction<'_>,
    goal_run_id: &str,
    task_id: &str,
    current: &GoalTaskState,
    next: &GoalTaskState,
) -> Result<(), String> {
    if current == next {
        return Ok(());
    }
    let permitted = matches!(
        (current, next),
        (GoalTaskState::Planned, GoalTaskState::Ready)
            | (GoalTaskState::Ready, GoalTaskState::Running)
            | (GoalTaskState::Running, GoalTaskState::NeedsEvidence)
            | (GoalTaskState::Running, GoalTaskState::Blocked)
            | (GoalTaskState::NeedsEvidence, GoalTaskState::Accepted)
            | (GoalTaskState::NeedsEvidence, GoalTaskState::Running)
            | (_, GoalTaskState::Cancelled)
    );
    if !permitted {
        return Err("goal task transition is not allowed".into());
    }
    if matches!(next, GoalTaskState::Running) && !dependencies_accepted(tx, goal_run_id, task_id)? {
        return Err("goal task dependencies are not accepted".into());
    }
    if matches!(next, GoalTaskState::Accepted) {
        let evidence_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM goal_task_evidence WHERE goal_run_id=?1 AND task_id=?2",
                params![goal_run_id, task_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("count goal task evidence: {error}"))?;
        if evidence_count == 0 {
            return Err("goal task needs accepted evidence before completion".into());
        }
    }
    Ok(())
}

fn dependencies_accepted(
    tx: &Transaction<'_>,
    goal_run_id: &str,
    task_id: &str,
) -> Result<bool, String> {
    let unresolved: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM goal_task_dependencies AS dependency
             JOIN goal_tasks AS task
               ON task.goal_run_id=dependency.goal_run_id
              AND task.task_id=dependency.dependency_task_id
             WHERE dependency.goal_run_id=?1 AND dependency.task_id=?2
               AND task.state!='accepted'",
            params![goal_run_id, task_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("check goal task dependencies: {error}"))?;
    Ok(unresolved == 0)
}

fn release_ready_dependents(tx: &Transaction<'_>, goal_run_id: &str) -> Result<(), String> {
    let mut statement = tx
        .prepare("SELECT task_id FROM goal_tasks WHERE goal_run_id=?1 AND state='planned'")
        .map_err(|error| format!("prepare ready goal task release: {error}"))?;
    let planned = statement
        .query_map([goal_run_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("read planned goal tasks: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("decode planned goal tasks: {error}"))?;
    drop(statement);
    for task_id in planned {
        if dependencies_accepted(tx, goal_run_id, &task_id)? {
            tx.execute(
                "UPDATE goal_tasks SET state='ready' WHERE goal_run_id=?1 AND task_id=?2",
                params![goal_run_id, task_id],
            )
            .map_err(|error| format!("release ready goal task: {error}"))?;
        }
    }
    Ok(())
}

fn refresh_goal_state(tx: &Transaction<'_>, goal_run_id: &str) -> Result<(), String> {
    let states = {
        let mut statement = tx
            .prepare("SELECT state FROM goal_tasks WHERE goal_run_id=?1")
            .map_err(|error| format!("prepare goal task state refresh: {error}"))?;
        let rows = statement
            .query_map([goal_run_id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("read goal task states: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode goal task states: {error}"))?;
        rows
    };
    let next = if states.iter().all(|state| state == "accepted") {
        GoalRunState::Completed
    } else if states.iter().any(|state| state == "blocked") {
        GoalRunState::NeedsGuidance
    } else if states
        .iter()
        .any(|state| state == "running" || state == "needs_evidence")
    {
        GoalRunState::Running
    } else {
        GoalRunState::Ready
    };
    tx.execute(
        "UPDATE goal_runs SET state=?2, updated_at_ms=?3 WHERE goal_run_id=?1",
        params![goal_run_id, next.as_str(), now_ms()],
    )
    .map_err(|error| format!("refresh goal run state: {error}"))?;
    Ok(())
}

fn load_goal_run(conn: &Connection, goal_run_id: &str) -> Result<Option<GoalRunRecord>, String> {
    let row = conn
        .query_row(
            "SELECT goal_run_id, channel_id, session_scope, source_event_id,
                    thread_root_event_id, goal, state, scope_version, max_parallel,
                    created_at_ms, updated_at_ms
             FROM goal_runs WHERE goal_run_id=?1",
            [goal_run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, u32>(7)?,
                    row.get::<_, u8>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("read goal run: {error}"))?;
    let Some((
        goal_run_id,
        channel_id,
        session_scope,
        source_event_id,
        thread_root_event_id,
        goal,
        state,
        scope_version,
        max_parallel,
        created_at_ms,
        updated_at_ms,
    )) = row
    else {
        return Ok(None);
    };
    let mut statement = conn
        .prepare(
            "SELECT task_id, ordinal, title, instructions, acceptance_criteria, state, generation
             FROM goal_tasks WHERE goal_run_id=?1 ORDER BY ordinal",
        )
        .map_err(|error| format!("prepare goal task read: {error}"))?;
    let task_rows = statement
        .query_map([&goal_run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)? as usize,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, u32>(6)?,
            ))
        })
        .map_err(|error| format!("read goal tasks: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("decode goal tasks: {error}"))?;
    drop(statement);
    let mut tasks = Vec::with_capacity(task_rows.len());
    for (task_id, ordinal, title, instructions, acceptance_criteria, task_state, generation) in
        task_rows
    {
        let dependencies = conn
            .prepare(
                "SELECT dependency_task_id FROM goal_task_dependencies
                 WHERE goal_run_id=?1 AND task_id=?2 ORDER BY dependency_task_id",
            )
            .map_err(|error| format!("prepare goal dependencies: {error}"))?
            .query_map(params![&goal_run_id, &task_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("read goal dependencies: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode goal dependencies: {error}"))?;
        let evidence = conn
            .prepare(
                "SELECT evidence_id, kind, reference, created_at_ms FROM goal_task_evidence
                 WHERE goal_run_id=?1 AND task_id=?2 ORDER BY created_at_ms, evidence_id",
            )
            .map_err(|error| format!("prepare goal evidence: {error}"))?
            .query_map(params![&goal_run_id, &task_id], |row| {
                Ok(GoalTaskEvidence {
                    evidence_id: row.get(0)?,
                    kind: row.get(1)?,
                    reference: row.get(2)?,
                    created_at_ms: row.get(3)?,
                })
            })
            .map_err(|error| format!("read goal evidence: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode goal evidence: {error}"))?;
        let assigned_agent_pubkey = conn
            .query_row(
                "SELECT assigned_agent_pubkey FROM goal_task_sources
                 WHERE goal_run_id=?1 AND task_id=?2 AND generation=?3
                 ORDER BY source_event_id LIMIT 1",
                params![&goal_run_id, &task_id, generation],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("read goal task assignee: {error}"))?
            .filter(|value| !value.is_empty());
        let attempt_turn_ids = conn
            .prepare(
                "SELECT turn_id FROM goal_task_attempts
                 WHERE goal_run_id=?1 AND task_id=?2 ORDER BY turn_id",
            )
            .map_err(|error| format!("prepare goal task attempts: {error}"))?
            .query_map(params![&goal_run_id, &task_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("read goal task attempts: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode goal task attempts: {error}"))?;
        tasks.push(GoalTaskRecord {
            task_id,
            ordinal,
            title,
            instructions,
            acceptance_criteria,
            depends_on: dependencies,
            state: GoalTaskState::parse(&task_state)?,
            generation,
            assigned_agent_pubkey,
            attempt_turn_ids,
            evidence,
        });
    }
    Ok(Some(GoalRunRecord {
        goal_run_id,
        channel_id,
        session_scope,
        source_event_id,
        thread_root_event_id,
        goal,
        state: GoalRunState::parse(&state)?,
        scope_version,
        max_parallel,
        tasks,
        created_at_ms,
        updated_at_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(source: char) -> GoalRunSpec {
        GoalRunSpec {
            channel_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            session_scope: "conversation".into(),
            source_event_id: source.to_string().repeat(64),
            thread_root_event_id: None,
            goal: "Build a small verified feature".into(),
            max_parallel: 2,
            tasks: vec![
                GoalTaskSpec {
                    title: "Implement".into(),
                    instructions: "Make the focused change.".into(),
                    acceptance_criteria: "The change has a check.".into(),
                    depends_on: vec![],
                },
                GoalTaskSpec {
                    title: "Verify".into(),
                    instructions: "Review the implementation evidence.".into(),
                    acceptance_criteria: "Evidence is attached.".into(),
                    depends_on: vec![0],
                },
            ],
        }
    }

    fn journal() -> (tempfile::TempDir, RunJournal) {
        let nest = tempfile::tempdir().unwrap();
        let journal =
            RunJournal::open_scoped(nest.path(), "wss://relay.example.test", &"b".repeat(64))
                .unwrap();
        (nest, journal)
    }

    #[test]
    fn goal_run_is_idempotent_and_requires_evidence_for_completion() {
        let (_nest, journal) = journal();
        let created = journal.create_goal_run(spec('a')).unwrap();
        let repeated = journal.create_goal_run(spec('a')).unwrap();
        assert_eq!(created.goal_run_id, repeated.goal_run_id);
        assert_eq!(created.state, GoalRunState::Ready);
        assert_eq!(created.tasks[0].state, GoalTaskState::Ready);
        assert_eq!(created.tasks[1].state, GoalTaskState::Planned);

        let first = &created.tasks[0];
        journal
            .transition_goal_task(
                &created.goal_run_id,
                &first.task_id,
                first.generation,
                GoalTaskState::Running,
            )
            .unwrap();
        journal
            .transition_goal_task(
                &created.goal_run_id,
                &first.task_id,
                first.generation,
                GoalTaskState::NeedsEvidence,
            )
            .unwrap();
        let error = journal
            .transition_goal_task(
                &created.goal_run_id,
                &first.task_id,
                first.generation,
                GoalTaskState::Accepted,
            )
            .unwrap_err();
        assert!(error.contains("evidence"));
        journal
            .append_goal_task_evidence(
                &created.goal_run_id,
                &first.task_id,
                first.generation,
                "validation",
                "cargo test -p focused-package",
            )
            .unwrap();
        let after_first = journal
            .transition_goal_task(
                &created.goal_run_id,
                &first.task_id,
                first.generation,
                GoalTaskState::Accepted,
            )
            .unwrap();
        let second = &after_first.tasks[1];
        assert_eq!(second.state, GoalTaskState::Ready);
        journal
            .transition_goal_task(
                &after_first.goal_run_id,
                &second.task_id,
                second.generation,
                GoalTaskState::Running,
            )
            .unwrap();
        journal
            .transition_goal_task(
                &after_first.goal_run_id,
                &second.task_id,
                second.generation,
                GoalTaskState::NeedsEvidence,
            )
            .unwrap();
        journal
            .append_goal_task_evidence(
                &after_first.goal_run_id,
                &second.task_id,
                second.generation,
                "artifact",
                "work/buzz/target/check-result.txt",
            )
            .unwrap();
        let complete = journal
            .transition_goal_task(
                &after_first.goal_run_id,
                &second.task_id,
                second.generation,
                GoalTaskState::Accepted,
            )
            .unwrap();
        assert_eq!(complete.state, GoalRunState::Completed);
    }

    #[test]
    fn invalid_goal_graph_does_not_write_a_partial_run() {
        let (_nest, journal) = journal();
        let mut invalid = spec('c');
        invalid.tasks[0].depends_on = vec![1];
        let error = journal.create_goal_run(invalid).unwrap_err();
        assert!(error.contains("cycle"));
        assert!(journal
            .recent_goal_runs("123e4567-e89b-12d3-a456-426614174000", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn assignment_source_links_the_exact_acp_trigger_to_its_task() {
        let (_nest, journal) = journal();
        let run = journal.create_goal_run(spec('d')).unwrap();
        let task = &run.tasks[0];
        let source = "e".repeat(64);
        journal
            .bind_goal_task_assignment_source(
                &run.goal_run_id,
                &task.task_id,
                task.generation,
                &source,
                &"a".repeat(64),
            )
            .unwrap();
        let mut conn = journal.connect().unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let turn_id = "123e4567-e89b-12d3-a456-426614174099";
        link_goal_task_attempts_for_turn(&tx, turn_id, &[source]).unwrap();
        tx.commit().unwrap();
        let linked = journal.goal_run(&run.goal_run_id).unwrap().unwrap();
        assert_eq!(linked.tasks[0].attempt_turn_ids, vec![turn_id.to_string()]);
    }

    #[test]
    fn only_the_assigned_agent_can_ingest_a_report() {
        let (_nest, journal) = journal();
        let run = journal.create_goal_run(spec('f')).unwrap();
        let task = &run.tasks[0];
        journal
            .bind_goal_task_assignment_source(
                &run.goal_run_id,
                &task.task_id,
                task.generation,
                &"e".repeat(64),
                &"a".repeat(64),
            )
            .unwrap();
        journal
            .transition_goal_task(
                &run.goal_run_id,
                &task.task_id,
                task.generation,
                GoalTaskState::Running,
            )
            .unwrap();
        let rejected = journal
            .ingest_goal_task_report(
                &run.goal_run_id,
                &task.task_id,
                task.generation,
                &"b".repeat(64),
                &"c".repeat(64),
                GoalTaskState::NeedsEvidence,
                &["tests passed".into()],
            )
            .unwrap_err();
        assert!(rejected.contains("assigned"));
        let reported = journal
            .ingest_goal_task_report(
                &run.goal_run_id,
                &task.task_id,
                task.generation,
                &"a".repeat(64),
                &"c".repeat(64),
                GoalTaskState::NeedsEvidence,
                &["cargo test -p focused-package".into()],
            )
            .unwrap();
        assert_eq!(reported.tasks[0].state, GoalTaskState::NeedsEvidence);
        assert_eq!(reported.tasks[0].evidence.len(), 2);
    }

    #[test]
    fn assigned_planner_can_append_one_bounded_dag() {
        let (_nest, journal) = journal();
        let mut root = spec('9');
        root.tasks.truncate(1);
        root.tasks[0].title = "Plan the work".into();
        let run = journal.create_goal_run(root).unwrap();
        let planner = &run.tasks[0];
        journal
            .bind_goal_task_assignment_source(
                &run.goal_run_id,
                &planner.task_id,
                planner.generation,
                &"e".repeat(64),
                &"a".repeat(64),
            )
            .unwrap();
        journal
            .transition_goal_task(
                &run.goal_run_id,
                &planner.task_id,
                planner.generation,
                GoalTaskState::Running,
            )
            .unwrap();
        let plan = vec![
            GoalTaskSpec {
                title: "Build".into(),
                instructions: "Implement the feature.".into(),
                acceptance_criteria: "Artifact exists.".into(),
                depends_on: vec![],
            },
            GoalTaskSpec {
                title: "Check".into(),
                instructions: "Validate the artifact.".into(),
                acceptance_criteria: "Check receipt exists.".into(),
                depends_on: vec![0],
            },
        ];
        let rejected = journal
            .append_goal_plan(
                &run.goal_run_id,
                &planner.task_id,
                planner.generation,
                &"b".repeat(64),
                &plan,
            )
            .unwrap_err();
        assert!(rejected.contains("assigned"));
        let extended = journal
            .append_goal_plan(
                &run.goal_run_id,
                &planner.task_id,
                planner.generation,
                &"a".repeat(64),
                &plan,
            )
            .unwrap();
        assert_eq!(extended.scope_version, 2);
        assert_eq!(extended.tasks.len(), 3);
        assert!(extended.tasks[1].depends_on.contains(&planner.task_id));
        let reported = journal
            .ingest_goal_task_report(
                &run.goal_run_id,
                &planner.task_id,
                planner.generation,
                &"a".repeat(64),
                &"c".repeat(64),
                GoalTaskState::NeedsEvidence,
                &["plan receipt".into()],
            )
            .unwrap();
        let accepted = journal
            .transition_goal_task(
                &reported.goal_run_id,
                &planner.task_id,
                planner.generation,
                GoalTaskState::Accepted,
            )
            .unwrap();
        assert_eq!(accepted.tasks[1].state, GoalTaskState::Ready);
        assert_eq!(accepted.tasks[2].state, GoalTaskState::Planned);
    }
}
