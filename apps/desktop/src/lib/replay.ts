/**
 * Restoring yesterday's conversation.
 *
 * Everything here is a pure function over persisted data — tasks, their
 * event streams, stored messages, and artifacts — so that reopening a
 * conversation reconstructs exactly what the user saw, and so every rule
 * is testable without a DOM or a backend.
 */
import type {
  AgentEvent,
  Artifact,
  Message,
  OperationResult,
  TaskState,
} from "@commonspace/protocol";
import { applyEvent, emptyConversationState, type ConversationState } from "./activity";

/**
 * The slice of a persisted task the replay logic needs. `TaskInfo` from
 * `lib/ipc.ts` satisfies this structurally; the live path can also
 * synthesize one from streaming state when a task finishes on screen.
 */
export interface ReplayTask {
  id: string;
  state: TaskState;
  summary?: string | null | undefined;
  error_message?: string | null | undefined;
}

/** The newest task by creation time (ties go to the later list position). */
export function newestTask<T extends { created_at: string }>(tasks: T[]): T | undefined {
  let newest: T | undefined;
  for (const task of tasks) {
    if (!newest || task.created_at.localeCompare(newest.created_at) >= 0) {
      newest = task;
    }
  }
  return newest;
}

/** Fold a persisted event stream through the live reducer. */
export function foldEvents(events: AgentEvent[]): ConversationState {
  return events.reduce(applyEvent, emptyConversationState());
}

/**
 * Rebuild the conversation state a reopened conversation should show.
 *
 * Two replay-only corrections on top of the plain fold:
 *
 * 1. Assistant text de-duplication. When a task's event stream ends inside
 *    a live app process, the orchestrator concatenates every
 *    `message.delta` and appends it to the conversation's `messages` as an
 *    assistant message (`crates/commonspace-runtime/src/orchestrator.rs`,
 *    the event-consumer task in `Orchestrator::start`: it accumulates
 *    `assistant_text` and calls `storage.append_message(...,
 *    MessageRole::Assistant, ...)` once the channel closes). That covers
 *    completed, failed, and cancelled tasks alike — so for those, messages
 *    are the durable record and rendering the replayed `assistantText` too
 *    would show the reply twice. The one case where the append never runs
 *    is a crash: the process died mid-task, crash recovery
 *    (`recover_after_restart` + `Storage::fail_task_for_recovery`) only
 *    flips the task to failed, and the events table is the only copy of
 *    the text. So the rule is evidence-based rather than state-based:
 *    suppress the replayed text exactly when an identical assistant
 *    message is already stored.
 *
 * 2. A replayed permission prompt is never actionable — the broker that
 *    could answer it died with the task — so it must not resurrect as a
 *    live question. Denials still show, as `denied` activity items.
 */
export function replayConversationState(
  events: AgentEvent[],
  messages: Message[],
): ConversationState {
  const state = foldEvents(events);
  const persisted =
    state.assistantText.length > 0 &&
    messages.some((m) => m.role === "assistant" && m.content === state.assistantText);
  return {
    ...state,
    assistantText: persisted ? "" : state.assistantText,
    pendingPermission: undefined,
  };
}

/* -------------------------------------------------- artifact aggregation */

/** One task's artifacts, kept grouped so undo-task knows its scope. */
export interface TaskArtifacts {
  taskId: string;
  artifacts: Artifact[];
}

/**
 * Flatten per-task artifact groups (ordered oldest task first) into one
 * panel list. Duplicate ids collapse to the first occurrence; when a later
 * task touches the same path again, the newer artifact replaces the older
 * one so a file appears once, with its most recent change and undo record.
 */
export function aggregateArtifacts(groups: TaskArtifacts[]): Artifact[] {
  const seen = new Set<string>();
  let out: Artifact[] = [];
  for (const group of groups) {
    for (const artifact of group.artifacts) {
      if (seen.has(artifact.id)) continue;
      seen.add(artifact.id);
      out = out.filter((existing) => existing.path !== artifact.path);
      out.push(artifact);
    }
  }
  return out;
}

/* --------------------------------------------------------- task outcomes */

export type OutcomeKind = "completed" | "failed" | "cancelled" | "interrupted" | "none";

export interface TaskOutcome {
  kind: OutcomeKind;
  taskId: string;
  /** Card heading, e.g. "What Commonspace did". */
  headline: string;
  /** The task summary, for completed tasks. */
  summary: string | undefined;
  /** The failure, for failed tasks. */
  error: { message: string; recovery: string | undefined } | undefined;
  /** Present when the user declined a permission during the task. */
  deniedNote: string | undefined;
  /** A short explanatory line (interruption, stop, or "no files"). */
  note: string | undefined;
  /** This task's artifacts only — the scope "Undo this task" acts on. */
  artifacts: Artifact[];
  canUndoTask: boolean;
}

/**
 * Crash recovery marks a task failed with this explanation
 * (`Orchestrator::recover_after_restart` passes it to
 * `Storage::fail_task_for_recovery`, which stores it as the task's error
 * message with code "interrupted"). Matching the prefix is how a replayed
 * "failed" task is recognized as interrupted rather than genuinely failed.
 */
const CRASH_RECOVERY_PREFIX = "Commonspace closed while this task was running";

/** States that mean the task was live when the app last stopped. */
const LIVE_STATES: readonly TaskState[] = [
  "planning",
  "awaiting_approval",
  "running",
  "paused",
];

export function isInterruptedTask(task: ReplayTask): boolean {
  if (LIVE_STATES.includes(task.state)) return true;
  return (
    task.state === "failed" && (task.error_message ?? "").startsWith(CRASH_RECOVERY_PREFIX)
  );
}

const DENIED_NOTE = "You declined a change, so Commonspace stopped there.";

/**
 * Derive the outcome card for a task from its persisted row, its replayed
 * (or live) conversation state, and the artifacts on record. Sections
 * without data stay undefined — the card omits them, never invents them.
 */
export function deriveOutcome(
  task: ReplayTask,
  state: ConversationState,
  artifacts: Artifact[],
): TaskOutcome {
  const own = artifacts.filter((artifact) => artifact.task_id === task.id);
  const deniedNote = state.activity.some((item) => item.status === "denied")
    ? DENIED_NOTE
    : undefined;
  const base: TaskOutcome = {
    kind: "none",
    taskId: task.id,
    headline: "",
    summary: undefined,
    error: undefined,
    deniedNote,
    note: undefined,
    artifacts: own,
    canUndoTask: own.some((artifact) => Boolean(artifact.file_operation_id)),
  };

  if (isInterruptedTask(task)) {
    const explanation =
      task.state === "failed" && task.error_message
        ? task.error_message
        : "Commonspace closed while this task was running, so it stopped.";
    return {
      ...base,
      kind: "interrupted",
      headline: "This task was interrupted.",
      note: `${explanation} Send a new message to continue the work.`,
    };
  }

  switch (task.state) {
    case "completed":
      return {
        ...base,
        kind: "completed",
        headline: "What Commonspace did",
        summary: state.summary ?? task.summary ?? undefined,
        note: own.length === 0 ? "No files were changed." : undefined,
      };
    case "failed": {
      const error = state.error ?? {
        message: task.error_message ?? "The task ended without a result.",
        recovery: undefined,
      };
      return {
        ...base,
        kind: "failed",
        headline: "This task didn't finish.",
        error: { message: error.message, recovery: error.recovery ?? undefined },
      };
    }
    case "cancelled":
      return { ...base, kind: "cancelled", headline: "You stopped this task." };
    case "rolled_back":
      return {
        ...base,
        kind: "cancelled",
        headline: "This task's changes were undone.",
        canUndoTask: false,
      };
    default:
      // "draft" — nothing ran, nothing to report.
      return base;
  }
}

/**
 * One calm line summarizing an "Undo this task" run, honest about partial
 * failure (some operations can refuse — e.g. a file edited since).
 */
export function summarizeUndo(results: OperationResult[]): string {
  if (results.length === 0) return "There was nothing to undo.";
  const undone = results.filter((result) => result.success).length;
  const total = results.length;
  if (undone === total) {
    return total === 1 ? "1 change undone." : `${total} changes undone.`;
  }
  if (undone === 0) {
    return total === 1
      ? "This change could not be undone. The file may have changed since."
      : "These changes could not be undone. The files may have changed since.";
  }
  return `${undone} of ${total} changes undone. The rest could not be — those files may have changed since.`;
}
