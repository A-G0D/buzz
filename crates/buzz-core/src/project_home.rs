//! Resolve an authoritative NIP-MP project home from relay events.

use std::collections::HashMap;

use serde_json::Value;

/// Project identity bound to an authoritative home channel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectHomeInfo {
    /// Human-facing project name, falling back to the project slug.
    pub name: String,
    /// NIP-MP `d` tag value used in the project coordinate.
    pub slug: String,
    /// Lowercase public key of the project event signer.
    pub owner: String,
    /// Canonical `30621:<owner>:<slug>` project address.
    pub coordinate: String,
    /// Owner of the repository that authoritatively binds this channel.
    pub default_repo_owner: Option<String>,
    /// Identifier of the repository that authoritatively binds this channel.
    pub default_repo_id: Option<String>,
}

/// Resolve one listed project whose member repository authoritatively binds the channel.
///
/// A project's own `buzz-channel` is presentation metadata and cannot establish
/// authority. A candidate is accepted only when one of its `a` members resolves
/// to a live `kind:30617` whose first `buzz-channel` is `channel_id` and whose
/// owner (or `maintainers`) authorizes the project signer. Ambiguity fails closed.
pub fn pick_authoritative_project_home(
    project_events: &[Value],
    repo_events: &[Value],
    channel_id: &str,
) -> Option<ProjectHomeInfo> {
    let repos = authoritative_channel_repos(repo_events, channel_id);
    let mut matches = project_events.iter().filter_map(|event| {
        if event_is_unlisted(event) || !event_has_tag_value(event, "buzz-channel", channel_id) {
            return None;
        }
        let mut project = parse_project(event)?;
        let signer = project.owner.as_str();
        let authoritative_member = event
            .get("tags")?
            .as_array()?
            .iter()
            .filter_map(|tag| tag.as_array())
            .filter(|tag| tag.first().and_then(Value::as_str) == Some("a"))
            .filter_map(|tag| tag.get(1).and_then(Value::as_str))
            .filter_map(parse_repo_coord)
            .find(|(owner, id)| {
                repos
                    .get(&(owner.clone(), id.clone()))
                    .is_some_and(|maintainers| {
                        owner.eq_ignore_ascii_case(signer)
                            || maintainers.iter().any(|m| m.eq_ignore_ascii_case(signer))
                    })
            })?;
        project.default_repo_owner = Some(authoritative_member.0);
        project.default_repo_id = Some(authoritative_member.1);
        Some(project)
    });
    let home = matches.next()?;
    matches.next().is_none().then_some(home)
}

/// Resolve one authoritative project home after applying NIP-09 deletions and
/// selecting the newest returned addressable head for each coordinate.
///
/// Deletion events only affect an address when the tombstone signer matches
/// the addressable event signer. Callers still own relay reads and must bound
/// the event sets before passing them here. This projection compares event
/// fields but does not verify signatures; callers must use their validated
/// relay event path.
pub fn pick_current_authoritative_project_home(
    project_events: &[Value],
    repo_events: &[Value],
    deletion_events: &[Value],
    channel_id: &str,
) -> Option<ProjectHomeInfo> {
    let project_heads = latest_live_addressable_heads(project_events, deletion_events);
    let repo_heads = latest_live_addressable_heads(repo_events, deletion_events);
    pick_authoritative_project_home(&project_heads, &repo_heads, channel_id)
}

fn latest_live_addressable_heads(events: &[Value], deletion_events: &[Value]) -> Vec<Value> {
    let mut latest = HashMap::<String, Value>::new();
    for event in events {
        let Some(coordinate) = event_coordinate(event) else {
            continue;
        };
        if is_deleted_by_author(deletion_events, event, &coordinate) {
            continue;
        }
        let replace = latest.get(&coordinate).is_none_or(|existing| {
            match (
                event.get("created_at").and_then(Value::as_u64),
                existing.get("created_at").and_then(Value::as_u64),
            ) {
                (Some(incoming), Some(current)) if incoming != current => incoming > current,
                (Some(_), Some(_)) => event_id(event) < event_id(existing),
                _ => false,
            }
        });
        if replace {
            latest.insert(coordinate, event.clone());
        }
    }
    latest.into_values().collect()
}

fn event_coordinate(event: &Value) -> Option<String> {
    let kind = event.get("kind")?.as_u64()?;
    if !(30_000..40_000).contains(&kind) {
        return None;
    }
    let owner = event.get("pubkey")?.as_str()?.to_ascii_lowercase();
    let slug = first_tag_value(event, "d")?;
    let id = event_id(event);
    let has_valid_id = id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
    let has_valid_owner = owner.len() == 64 && owner.bytes().all(|byte| byte.is_ascii_hexdigit());
    let has_timestamp = event.get("created_at").and_then(Value::as_u64).is_some();
    (has_valid_id
        && has_valid_owner
        && has_timestamp
        && !slug.is_empty()
        && slug.len() <= 1024
        && !slug.chars().any(char::is_control))
    .then(|| format!("{kind}:{owner}:{slug}"))
}

fn event_id(event: &Value) -> &str {
    event.get("id").and_then(Value::as_str).unwrap_or_default()
}

fn is_deleted_by_author(deletions: &[Value], event: &Value, coordinate: &str) -> bool {
    let Some(author) = event.get("pubkey").and_then(Value::as_str) else {
        return false;
    };
    let created_at = event
        .get("created_at")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    deletions.iter().any(|deletion| {
        deletion.get("kind").and_then(Value::as_u64) == Some(5)
            && deletion
                .get("pubkey")
                .and_then(Value::as_str)
                .is_some_and(|signer| signer.eq_ignore_ascii_case(author))
            && deletion
                .get("created_at")
                .and_then(Value::as_u64)
                .is_some_and(|deleted_at| deleted_at >= created_at)
            && tag_values(deletion, "a").any(|target| target == coordinate)
    })
}

fn authoritative_channel_repos(
    events: &[Value],
    channel_id: &str,
) -> HashMap<(String, String), Vec<String>> {
    events
        .iter()
        .filter_map(|event| {
            if event.get("kind").and_then(Value::as_u64) != Some(30617)
                || event_is_unlisted(event)
                || first_tag_value(event, "buzz-channel") != Some(channel_id)
            {
                return None;
            }
            let owner = event.get("pubkey")?.as_str()?.trim().to_ascii_lowercase();
            if owner.len() != 64 {
                return None;
            }
            let id = first_tag_value(event, "d")?.trim();
            if id.is_empty() {
                return None;
            }
            let maintainers = multi_tag_values(event, "maintainers")
                .map(str::to_ascii_lowercase)
                .collect();
            Some(((owner, id.to_string()), maintainers))
        })
        .collect()
}

fn first_tag_value<'a>(event: &'a Value, name: &'static str) -> Option<&'a str> {
    tag_values(event, name).next()
}

fn tag_values<'a>(event: &'a Value, name: &'static str) -> impl Iterator<Item = &'a str> {
    event
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .filter(move |tag| tag.first().and_then(Value::as_str) == Some(name))
        .filter_map(|tag| tag.get(1).and_then(Value::as_str))
}

fn multi_tag_values<'a>(event: &'a Value, name: &'static str) -> impl Iterator<Item = &'a str> {
    event
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .filter(move |tag| tag.first().and_then(Value::as_str) == Some(name))
        .flat_map(|tag| tag.iter().skip(1).filter_map(Value::as_str))
}

fn event_has_tag_value(event: &Value, name: &'static str, value: &str) -> bool {
    tag_values(event, name).any(|candidate| candidate == value)
}

fn event_is_unlisted(event: &Value) -> bool {
    event_has_tag_value(event, "buzz-visibility", "unlisted")
}

fn parse_project(event: &Value) -> Option<ProjectHomeInfo> {
    if event.get("kind").and_then(Value::as_u64) != Some(30621) {
        return None;
    }
    let owner = event.get("pubkey")?.as_str()?.trim().to_ascii_lowercase();
    if owner.len() != 64 {
        return None;
    }
    let slug = first_tag_value(event, "d")?.trim().to_string();
    if slug.is_empty() {
        return None;
    }
    let name = first_tag_value(event, "name")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&slug)
        .to_string();
    Some(ProjectHomeInfo {
        name,
        coordinate: format!("30621:{owner}:{slug}"),
        slug,
        owner,
        default_repo_owner: None,
        default_repo_id: None,
    })
}

fn parse_repo_coord(value: &str) -> Option<(String, String)> {
    let mut parts = value.splitn(3, ':');
    let kind = parts.next()?;
    let owner = parts.next()?.trim().to_ascii_lowercase();
    let id = parts.next()?.trim();
    if kind != "30617" || owner.len() != 64 || id.is_empty() {
        return None;
    }
    Some((owner, id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHANNEL_ID: &str = "11111111-1111-4111-8111-111111111111";

    fn project(owner: &str, slug: &str, repo: &str) -> Value {
        json!({"id": "1".repeat(64), "pubkey": owner, "kind": 30621, "created_at": 100, "tags": [
            ["d", slug], ["name", slug], ["buzz-channel", CHANNEL_ID], ["a", repo]
        ]})
    }

    fn repo(owner: &str, id: &str, channel: &str, extra: Vec<Value>) -> Value {
        let mut tags = vec![json!(["d", id]), json!(["buzz-channel", channel])];
        tags.extend(extra);
        json!({"id": "2".repeat(64), "pubkey": owner, "kind": 30617, "created_at": 100, "tags": tags})
    }

    #[test]
    fn requires_repo_owned_channel_binding() {
        let owner = "a".repeat(64);
        let coord = format!("30617:{owner}:game");
        let home = pick_authoritative_project_home(
            &[project(&owner, "game", &coord)],
            &[repo(&owner, "game", CHANNEL_ID, vec![])],
            CHANNEL_ID,
        )
        .unwrap();
        assert_eq!(home.default_repo_id.as_deref(), Some("game"));

        assert!(pick_authoritative_project_home(
            &[project(&owner, "game", &coord)],
            &[],
            CHANNEL_ID
        )
        .is_none());
    }

    #[test]
    fn hostile_project_cannot_claim_foreign_repo() {
        let owner = "a".repeat(64);
        let attacker = "b".repeat(64);
        let coord = format!("30617:{owner}:game");
        assert!(pick_authoritative_project_home(
            &[project(&attacker, "spoof", &coord)],
            &[repo(&owner, "game", CHANNEL_ID, vec![])],
            CHANNEL_ID,
        )
        .is_none());
    }

    #[test]
    fn repo_maintainer_can_authorize_project() {
        let owner = "a".repeat(64);
        let maintainer = "b".repeat(64);
        let coord = format!("30617:{owner}:game");
        let home = pick_authoritative_project_home(
            &[project(&maintainer, "suite", &coord)],
            &[repo(
                &owner,
                "game",
                CHANNEL_ID,
                vec![json!(["maintainers", "c".repeat(64), maintainer])],
            )],
            CHANNEL_ID,
        )
        .unwrap();
        assert_eq!(home.owner, maintainer);
    }

    #[test]
    fn ambiguous_authoritative_projects_fail_closed() {
        let owner = "a".repeat(64);
        let coord = format!("30617:{owner}:game");
        assert!(pick_authoritative_project_home(
            &[
                project(&owner, "one", &coord),
                project(&owner, "two", &coord)
            ],
            &[repo(&owner, "game", CHANNEL_ID, vec![])],
            CHANNEL_ID,
        )
        .is_none());
    }

    #[test]
    fn current_project_home_uses_latest_head_and_signer_scoped_deletions() {
        let owner = "a".repeat(64);
        let other = "b".repeat(64);
        let channel = CHANNEL_ID;
        let coordinate = format!("30617:{owner}:game");
        let old = project(&owner, "game", &coordinate);
        let moved = {
            let mut event = project(&owner, "game", &coordinate);
            event["created_at"] = json!(102);
            event["tags"][2] = json!(["buzz-channel", "22222222-2222-4222-8222-222222222222"]);
            event
        };
        let repo_event = repo(&owner, "game", channel, vec![]);
        let repo_coordinate = format!("30617:{owner}:game");
        let tombstone = |signer: &str, created_at| {
            json!({"kind": 5, "pubkey": signer, "created_at": created_at,
                "tags": [["a", repo_coordinate]]})
        };

        // A stale matching project head cannot win over a newer home move.
        assert!(pick_current_authoritative_project_home(
            &[old.clone(), moved],
            std::slice::from_ref(&repo_event),
            &[],
            channel,
        )
        .is_none());

        // A different signer cannot hide the member repository.
        assert!(pick_current_authoritative_project_home(
            std::slice::from_ref(&old),
            std::slice::from_ref(&repo_event),
            &[tombstone(&other, 101)],
            channel,
        )
        .is_some());

        // The repository owner can delete the binding; a newer recreation restores it.
        assert!(pick_current_authoritative_project_home(
            std::slice::from_ref(&old),
            std::slice::from_ref(&repo_event),
            &[tombstone(&owner, 101)],
            channel,
        )
        .is_none());
        let recreated = {
            let mut event = repo_event.clone();
            event["created_at"] = json!(102);
            event
        };
        assert!(pick_current_authoritative_project_home(
            &[old],
            &[repo_event, recreated],
            &[tombstone(&owner, 101)],
            channel,
        )
        .is_some());
    }

    #[test]
    fn tied_moved_project_head_wins_by_canonical_id_order_independent_of_input_order() {
        let owner = "a".repeat(64);
        let coordinate = format!("30617:{owner}:game");
        let mut stale = project(&owner, "game", &coordinate);
        stale["id"] = json!("f".repeat(64));
        let mut moved = project(&owner, "game", &coordinate);
        moved["id"] = json!("a".repeat(64));
        moved["tags"][2] = json!(["buzz-channel", "22222222-2222-4222-8222-222222222222"]);
        let repo = repo(&owner, "game", CHANNEL_ID, vec![]);

        // Buzz orders replaceable heads by created_at DESC, then id ASC. The
        // lower-ID move must beat the stale channel claim, whichever arrives first.
        assert!(pick_current_authoritative_project_home(
            &[stale.clone(), moved.clone()],
            std::slice::from_ref(&repo),
            &[],
            CHANNEL_ID,
        )
        .is_none());
        assert!(
            pick_current_authoritative_project_home(&[moved, stale], &[repo], &[], CHANNEL_ID,)
                .is_none()
        );
    }

    #[test]
    fn unlisted_current_project_is_not_an_authoritative_listed_home() {
        let owner = "a".repeat(64);
        let coordinate = format!("30617:{owner}:game");
        let mut project = project(&owner, "game", &coordinate);
        project["tags"]
            .as_array_mut()
            .unwrap()
            .push(json!(["buzz-visibility", "unlisted"]));
        assert!(pick_current_authoritative_project_home(
            &[project],
            &[repo(&owner, "game", CHANNEL_ID, vec![])],
            &[],
            CHANNEL_ID,
        )
        .is_none());
    }

    #[test]
    fn first_repo_channel_binding_is_authoritative() {
        let owner = "a".repeat(64);
        let coord = format!("30617:{owner}:game");
        let other = "22222222-2222-4222-8222-222222222222";
        let mut announcement = repo(&owner, "game", other, vec![]);
        announcement["tags"]
            .as_array_mut()
            .unwrap()
            .push(json!(["buzz-channel", CHANNEL_ID]));
        assert!(pick_authoritative_project_home(
            &[project(&owner, "game", &coord)],
            &[announcement],
            CHANNEL_ID
        )
        .is_none());
    }
}
