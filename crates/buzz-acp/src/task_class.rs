//! Owner-authored per-turn task-class metadata carried by kind-9 events.
//!
//! An ACP turn is classified only when every event merged into that turn has
//! the same canonical tag, verifies cryptographically, and is signed directly
//! by the configured agent owner. Missing, mixed, malformed, or non-owner
//! input stays unknown.

use nostr::{Event, Kind, Tag};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::queue::FlushBatch;

pub(crate) const METADATA_VERSION: u32 = 1;
pub(crate) const TAXONOMY_VERSION: &str = "operator-defined-v1";
pub(crate) const DESKTOP_SOURCE: &str = "desktop_ui";
pub(crate) const CLI_SOURCE: &str = "cli_explicit";
const TAG_NAME: &str = "buzz:task-class";
const SUPPORTED_TASK_CLASSES: &[&str] = &[
    "coding",
    "code_review",
    "research",
    "writing",
    "analysis",
    "planning",
    "summarization",
    "classification",
];

/// Metadata accepted from explicit operator task-class controls.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TaskClassMetadata {
    pub version: u32,
    pub task_class: String,
    pub taxonomy_version: String,
    pub source: String,
}

impl TaskClassMetadata {
    pub(crate) fn validate(&self) -> bool {
        self.version == METADATA_VERSION
            && self.taxonomy_version == TAXONOMY_VERSION
            && matches!(self.source.as_str(), DESKTOP_SOURCE | CLI_SOURCE)
            && SUPPORTED_TASK_CLASSES.contains(&self.task_class.as_str())
    }
}

/// Read metadata only when every current and cancelled event is one verified,
/// homogeneous owner-authored operator submission.
/// A mixed FIFO batch remains unknown as a whole; queue-level class partitioning
/// is deferred, so strict routes abstain instead of guessing a class.
pub(crate) fn from_batch(
    batch: Option<&FlushBatch>,
    owner: Option<&nostr::PublicKey>,
) -> Option<TaskClassMetadata> {
    let batch = batch?;
    let owner = owner?;
    let mut selected: Option<TaskClassMetadata> = None;
    let mut count = 0usize;

    for event in batch
        .events
        .iter()
        .chain(batch.cancelled_events.iter())
        .map(|event| &event.event)
    {
        count += 1;
        let metadata = from_event(event, batch.channel_id, owner)?;
        if selected
            .as_ref()
            .is_some_and(|previous| previous != &metadata)
        {
            return None;
        }
        selected = Some(metadata);
    }

    (count > 0)
        .then_some(selected?)
        .filter(TaskClassMetadata::validate)
}

fn from_event(
    event: &Event,
    channel_id: Uuid,
    owner: &nostr::PublicKey,
) -> Option<TaskClassMetadata> {
    // Relay intake already verifies each NIP-01 event before queue admission.
    // Recheck here because this tag is privileged routing metadata.
    event.verify().ok()?;
    if event.pubkey != *owner || event.kind != Kind::Custom(9) {
        return None;
    }

    let channel = channel_id.to_string();
    let channel_tags: Vec<_> = event
        .tags
        .iter()
        .filter(|tag| tag.kind().to_string() == "h")
        .collect();
    if channel_tags.len() != 1 || channel_tags[0].content() != Some(channel.as_str()) {
        return None;
    }

    let task_tags: Vec<_> = event
        .tags
        .iter()
        .map(Tag::as_slice)
        .filter(|parts| parts.first().is_some_and(|part| part == TAG_NAME))
        .collect();
    let [parts] = task_tags.as_slice() else {
        return None;
    };
    let parts = *parts;
    let [name, version, taxonomy_version, source, task_class] = parts else {
        return None;
    };
    if name != TAG_NAME || version != "1" {
        return None;
    }
    let metadata = TaskClassMetadata {
        version: METADATA_VERSION,
        task_class: task_class.to_owned(),
        taxonomy_version: taxonomy_version.to_owned(),
        source: source.to_owned(),
    };
    metadata.validate().then_some(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{queue::BatchEvent, scope::SessionScope};
    use nostr::Keys;
    use std::time::Instant;

    const CHANNEL: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    fn event(keys: &Keys, task_class: Option<&str>) -> Event {
        event_with_source(keys, task_class, DESKTOP_SOURCE)
    }

    fn event_with_source(keys: &Keys, task_class: Option<&str>, source: &str) -> Event {
        let mut tags = vec![Tag::parse(["h", CHANNEL]).expect("channel tag")];
        if let Some(task_class) = task_class {
            tags.push(
                Tag::parse([TAG_NAME, "1", TAXONOMY_VERSION, source, task_class])
                    .expect("task class tag"),
            );
        }
        nostr::EventBuilder::new(Kind::Custom(9), "user message")
            .tags(tags)
            .sign_with_keys(keys)
            .expect("signed event")
    }

    fn batch(events: Vec<Event>) -> FlushBatch {
        FlushBatch {
            channel_id: CHANNEL.parse().expect("channel UUID"),
            scope: SessionScope::Conversation {
                channel_id: CHANNEL.parse().expect("channel UUID"),
            },
            events: events
                .into_iter()
                .map(|event| BatchEvent {
                    event,
                    prompt_tag: "@mention".into(),
                    received_at: Instant::now(),
                })
                .collect(),
            cancelled_events: vec![],
            cancel_reason: None,
        }
    }

    #[test]
    fn accepts_one_verified_class_from_the_exact_owner() {
        let owner = Keys::generate();
        let coding_event = event(&owner, Some("coding"));
        let coding_batch = batch(vec![coding_event]);
        let metadata =
            from_batch(Some(&coding_batch), Some(&owner.public_key())).expect("metadata");
        assert_eq!(metadata.task_class, "coding");
        assert_eq!(metadata.source, DESKTOP_SOURCE);

        let cli_batch = batch(vec![event_with_source(&owner, Some("coding"), CLI_SOURCE)]);
        let cli_metadata =
            from_batch(Some(&cli_batch), Some(&owner.public_key())).expect("CLI metadata");
        assert_eq!(cli_metadata.task_class, "coding");
        assert_eq!(cli_metadata.source, CLI_SOURCE);

        let underscore_class = event(&owner, Some("code_review"));
        let underscore_batch = batch(vec![underscore_class]);
        assert_eq!(
            from_batch(Some(&underscore_batch), Some(&owner.public_key()))
                .expect("built-in underscore class")
                .task_class,
            "code_review"
        );
    }

    #[test]
    fn missing_non_owner_and_tampered_tags_are_unknown() {
        let owner = Keys::generate();
        let stranger = Keys::generate();
        assert!(from_batch(
            Some(&batch(vec![event(&owner, None)])),
            Some(&owner.public_key())
        )
        .is_none());
        assert!(from_batch(
            Some(&batch(vec![event(&stranger, Some("coding"))])),
            Some(&owner.public_key())
        )
        .is_none());
        assert!(from_batch(
            Some(&batch(vec![event(&owner, Some("unknown"))])),
            Some(&owner.public_key())
        )
        .is_none());
        assert!(from_batch(
            Some(&batch(vec![event(&owner, Some("operator_custom_class"))])),
            Some(&owner.public_key())
        )
        .is_none());

        let mut tampered = event(&owner, Some("coding"));
        tampered.content = "changed after signing".into();
        assert!(from_batch(Some(&batch(vec![tampered])), Some(&owner.public_key())).is_none());
    }

    #[test]
    fn mixed_or_duplicate_metadata_makes_the_entire_batch_unknown() {
        let owner = Keys::generate();
        let mixed = batch(vec![event(&owner, Some("coding")), event(&owner, None)]);
        assert!(from_batch(Some(&mixed), Some(&owner.public_key())).is_none());

        let mut cancelled_missing = batch(vec![event(&owner, Some("coding"))]);
        cancelled_missing
            .cancelled_events
            .push(crate::queue::BatchEvent {
                event: event(&owner, None),
                prompt_tag: "@mention".into(),
                received_at: Instant::now(),
            });
        assert!(from_batch(Some(&cancelled_missing), Some(&owner.public_key())).is_none());

        let mixed_sources = batch(vec![
            event(&owner, Some("coding")),
            event_with_source(&owner, Some("coding"), CLI_SOURCE),
        ]);
        assert!(from_batch(Some(&mixed_sources), Some(&owner.public_key())).is_none());

        let duplicate = nostr::EventBuilder::new(Kind::Custom(9), "user message")
            .tags(vec![
                Tag::parse(["h", CHANNEL]).expect("channel tag"),
                Tag::parse([TAG_NAME, "1", TAXONOMY_VERSION, DESKTOP_SOURCE, "coding"])
                    .expect("task class tag"),
                Tag::parse([TAG_NAME, "1", TAXONOMY_VERSION, DESKTOP_SOURCE, "coding"])
                    .expect("duplicate task class tag"),
            ])
            .sign_with_keys(&owner)
            .expect("owner-signed duplicate tags");
        duplicate.verify().expect("duplicate event itself is valid");
        assert!(from_batch(Some(&batch(vec![duplicate])), Some(&owner.public_key())).is_none());
    }
}
