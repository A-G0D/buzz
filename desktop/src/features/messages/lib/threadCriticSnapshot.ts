import type { TimelineMessage } from "@/features/messages/types";
import type { MainTimelineEntry } from "./threadPanel";
import type { ThreadBriefResponse } from "@/shared/api/types";

export const THREAD_CRITIC_SNAPSHOT_MAX_BYTES = 64 * 1024;
const THREAD_BRIEF_MAX_BYTES = 12 * 1024;

const STABLE_EVENT_ID = /^[0-9a-f]{64}$/i;
const SNAPSHOT_HEADER =
  "Buzz thread snapshot (frozen when review opened)\nOnly currently loaded messages are included. Relay history may be incomplete or stale. Later messages will not be added automatically.\nDisplayed bodies may reflect Buzz edit overlays; they are not necessarily raw signed event content.\n\n";
const TRUNCATION_NOTICE =
  "[Additional loaded messages omitted at a message boundary to stay within the 64 KiB limit.]\n";

export type ThreadCriticSnapshot = {
  snapshot: string;
  sourceIds: string[];
  includedMessageCount: number;
  omittedMessageCount: number;
  disclosure: string;
};

type ThreadCriticSnapshotInput = {
  head: TimelineMessage;
  replies: readonly MainTimelineEntry[];
  repliesPending?: boolean;
  repliesError?: boolean;
  brief?: ThreadBriefResponse | null;
  briefError?: boolean;
};

function messagePrefix(message: TimelineMessage): string {
  const sourceId = STABLE_EVENT_ID.test(message.id)
    ? message.id.toLowerCase()
    : "unavailable (no stable event ID)";
  const author =
    message.author.replace(/[\r\n\u0085\u2028\u2029]+/g, " ").trim() ||
    "Unknown author";
  const timestamp = new Date(message.createdAt * 1000);
  const timestampUtc = Number.isNaN(timestamp.getTime())
    ? "unavailable"
    : timestamp.toISOString();
  return `Source event ID: ${sourceId}\nOriginal event timestamp (UTC): ${timestampUtc}${message.edited ? " (edited)" : ""}\nAuthor: ${author}\n`;
}

function utf8ByteLength(value: string, stopAfter = Number.POSITIVE_INFINITY) {
  let length = 0;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x7f) {
      length += 1;
    } else if (code <= 0x7ff) {
      length += 2;
    } else if (
      code >= 0xd800 &&
      code <= 0xdbff &&
      index + 1 < value.length &&
      value.charCodeAt(index + 1) >= 0xdc00 &&
      value.charCodeAt(index + 1) <= 0xdfff
    ) {
      length += 4;
      index += 1;
    } else {
      // TextEncoder replaces unpaired surrogates with the three-byte U+FFFD.
      length += 3;
    }
    if (length > stopAfter) return length;
  }
  return length;
}

function truncateUtf8(value: string, maximumBytes: number) {
  const marker = "\n[Text shortened to fit the review snapshot.]";
  const markerBytes = utf8ByteLength(marker);
  const contentLimit = Math.max(0, maximumBytes - markerBytes);
  let bytes = 0;
  let result = "";
  for (const character of value) {
    const characterBytes = utf8ByteLength(character);
    if (bytes + characterBytes > contentLimit) {
      return `${result}${marker}`;
    }
    result += character;
    bytes += characterBytes;
  }
  return result;
}

function briefEventBlock(
  label: string,
  event: ThreadBriefResponse["original_intent"],
  contentLimit: number,
  sourceIds: string[],
) {
  const sourceId = STABLE_EVENT_ID.test(event.id)
    ? event.id.toLowerCase()
    : "unavailable (no stable event ID)";
  if (STABLE_EVENT_ID.test(event.id)) sourceIds.push(sourceId);
  const content = event.content || "(No text content)";
  return `${label}\nSource event ID: ${sourceId}\n${truncateUtf8(content, contentLimit)}`;
}

function buildBriefEvidence(
  head: TimelineMessage,
  brief: ThreadBriefResponse | null | undefined,
  briefError: boolean,
) {
  if (!brief || brief.thread_root_id.toLowerCase() !== head.id.toLowerCase()) {
    if (!briefError && !brief) return null;
    return {
      sourceIds: [] as string[],
      text: [
        "## Source-linked thread brief",
        "Could not load a matching source-linked thread brief. Only messages already loaded in the thread panel are included.",
      ].join("\n"),
      disclosure:
        "Could not load a matching source-linked thread brief; only messages already loaded in the panel are included.",
    };
  }

  const sourceIds: string[] = [];
  const recentProgress = brief.progress_events.slice(-3).reverse();
  const lines = [
    "## Source-linked thread brief",
    "Captured when review opened; this evidence does not refresh while the dialog is open.",
    briefEventBlock(
      "### Original intent",
      brief.original_intent,
      3072,
      sourceIds,
    ),
    "### Recent progress (up to three returned replies)",
  ];
  if (recentProgress.length === 0) {
    lines.push("No reply events were returned.");
  } else {
    for (const event of recentProgress) {
      lines.push(briefEventBlock("Progress event", event, 1536, sourceIds));
    }
  }

  const steeringControls = brief.status.steering_controls?.slice(-3) ?? [];
  if (steeringControls.length > 0) {
    lines.push("### Recorded steering evidence");
    for (const control of steeringControls) {
      const sourceId = STABLE_EVENT_ID.test(control.source_event_id)
        ? control.source_event_id.toLowerCase()
        : "unavailable (no stable event ID)";
      if (STABLE_EVENT_ID.test(control.source_event_id)) {
        sourceIds.push(sourceId);
      }
      lines.push(
        `Steering source event ID: ${sourceId}; recorded state: ${control.state}; agent observation: unknown.`,
      );
    }
  }

  const nextPage = brief.status.next_cursor
    ? "another reply page is available and was not loaded"
    : "no additional reply page was reported";
  const depth = brief.status.depth_limit_may_truncate
    ? `nested replies beyond depth ${brief.status.applied_depth_limit} may be omitted`
    : "no depth truncation was reported";
  const turnCount = brief.status.managed_turns.length;
  const runCount = brief.status.coordinator_runs.length;
  const captureGaps = brief.status.managed_turn_lookup?.capture_gap_count;
  lines.push(
    "### Evidence limits",
    `Replies returned: ${brief.status.reply_event_count}; ${nextPage}; ${depth}.`,
    `Thread evidence may be truncated: ${brief.status.possibly_truncated ? "yes" : "not reported"}.`,
    `Local evidence contains ${runCount} recorded run(s) and ${turnCount} managed turn(s); capture reliability is best-effort${typeof captureGaps === "number" ? ` with ${captureGaps} recorded journal gap(s)` : ""}.`,
    "Task completion: unknown. Worker liveness: unknown.",
  );

  const rawText = lines.join("\n\n");
  const text = truncateUtf8(rawText, THREAD_BRIEF_MAX_BYTES);
  const disclosure = [
    "A source-linked thread brief was fetched when review opened and is frozen with the snapshot.",
    `The brief includes the original intent and ${recentProgress.length} recent progress event(s) with source IDs.`,
    `Thread evidence may be truncated: ${brief.status.possibly_truncated ? "yes" : "not reported"}; ${nextPage}; ${depth}.`,
    "Task completion and worker liveness remain unknown.",
    utf8ByteLength(rawText) > THREAD_BRIEF_MAX_BYTES
      ? "Brief text was shortened to stay within the snapshot size limit."
      : null,
  ]
    .filter((line): line is string => line !== null)
    .join(" ");
  return { sourceIds, text, disclosure };
}

function messageByteLength(message: TimelineMessage, maximum: number) {
  const prefix = messagePrefix(message);
  const fixedBytes = utf8ByteLength(prefix) + 2;
  return fixedBytes + utf8ByteLength(message.body, maximum - fixedBytes);
}

function messageBlock(message: TimelineMessage): string {
  return `${messagePrefix(message)}${message.body}\n\n`;
}

/** Freeze only the head and reply rows currently held by the thread panel. */
function buildLoadedMessageSnapshot(
  { head, replies, repliesPending, repliesError }: ThreadCriticSnapshotInput,
  maximumBytes: number,
) {
  const seenIds = new Set<string>();
  const messages = [head, ...replies.map(({ message }) => message)].filter(
    (message) => {
      const dedupeId = STABLE_EVENT_ID.test(message.id)
        ? message.id.toLowerCase()
        : message.id;
      if (dedupeId && seenIds.has(dedupeId)) return false;
      if (dedupeId) seenIds.add(dedupeId);
      return true;
    },
  );
  let completeBytes = utf8ByteLength(SNAPSHOT_HEADER);
  for (const message of messages) {
    completeBytes += messageByteLength(message, maximumBytes - completeBytes);
    if (completeBytes > maximumBytes) break;
  }

  let includedMessages = messages;
  let includedMessageCount = messages.length;
  let snapshot: string;
  if (completeBytes <= maximumBytes) {
    snapshot = SNAPSHOT_HEADER + messages.map(messageBlock).join("");
  } else {
    let usedBytes = utf8ByteLength(SNAPSHOT_HEADER + TRUNCATION_NOTICE);
    includedMessages = [];
    for (const message of messages) {
      const blockBytes = messageByteLength(message, maximumBytes - usedBytes);
      if (usedBytes + blockBytes > maximumBytes) break;
      includedMessages.push(message);
      usedBytes += blockBytes;
    }
    includedMessageCount = includedMessages.length;
    snapshot =
      SNAPSHOT_HEADER +
      includedMessages.map(messageBlock).join("") +
      TRUNCATION_NOTICE;
  }

  const includedIds = includedMessages
    .map(({ id }) => id.toLowerCase())
    .filter((id) => STABLE_EVENT_ID.test(id));
  const omittedMessageCount = messages.length - includedMessageCount;
  const disclosure = [
    "This is frozen to only the messages currently loaded in Buzz when you opened the review. Relay history may be incomplete or stale, and later messages will not be added automatically.",
    repliesPending ? "Replies are still loading." : null,
    repliesError
      ? "Buzz could not load all replies; this snapshot may be incomplete."
      : null,
    omittedMessageCount > 0
      ? `${omittedMessageCount} loaded message${omittedMessageCount === 1 ? " was" : "s were"} omitted at a message boundary to stay within 64 KiB.`
      : null,
  ]
    .filter((line): line is string => line !== null)
    .join(" ");

  return {
    snapshot,
    sourceIds: includedIds,
    includedMessageCount,
    omittedMessageCount,
    disclosure,
  };
}

/** Freeze loaded messages and an optional exact-thread brief into one editable, byte-bounded snapshot. */
export function buildThreadCriticSnapshot(
  input: ThreadCriticSnapshotInput,
): ThreadCriticSnapshot {
  const { head, brief, briefError = false } = input;
  const evidence = buildBriefEvidence(head, brief, briefError);
  const separator = evidence ? "\n\n" : "";
  const messageBudget =
    THREAD_CRITIC_SNAPSHOT_MAX_BYTES -
    utf8ByteLength(separator) -
    (evidence ? utf8ByteLength(evidence.text) : 0);
  const loaded = buildLoadedMessageSnapshot(input, messageBudget);
  const snapshot = loaded.snapshot + separator + (evidence?.text ?? "");
  const sourceIds = [
    ...new Set([...loaded.sourceIds, ...(evidence?.sourceIds ?? [])]),
  ];
  return {
    snapshot,
    sourceIds,
    includedMessageCount: loaded.includedMessageCount,
    omittedMessageCount: loaded.omittedMessageCount,
    disclosure: [loaded.disclosure, evidence?.disclosure]
      .filter((line): line is string => Boolean(line))
      .join(" "),
  };
}
