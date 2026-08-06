/**
 * Turning the normalized event stream into a human-readable activity
 * timeline.
 *
 * The rule from the product brief: a person who has never opened a terminal
 * should understand what happened. Raw commands, tool names, and provider
 * payloads exist only behind "Technical details".
 */
import type { AgentEvent, Artifact, PermissionRequest, TaskPlan } from "@commonspace/protocol";

export type ActivityStatus = "running" | "done" | "failed" | "denied" | "info";

export interface ActivityItem {
  id: string;
  /** Plain-language line, e.g. "Reading 12 documents". */
  title: string;
  /** Optional second line with a small amount of detail. */
  detail?: string | undefined;
  status: ActivityStatus;
  /** Machine-facing information, shown only under Technical details. */
  technical?: string | undefined;
}

export interface ConversationState {
  /** Streaming assistant text, keyed by message id and concatenated in order. */
  assistantText: string;
  reasoning: string[];
  activity: ActivityItem[];
  artifacts: Artifact[];
  plan: TaskPlan | undefined;
  /**
   * True once execution evidence (a tool, artifact, completion, or error
   * event) has arrived after the most recent plan event. The plan-approval
   * derivation in `lib/replay.ts` reads this: a gating plan with nothing
   * executed after it is still waiting on the user's decision.
   */
  executionAfterPlan: boolean;
  pendingPermission: PermissionRequest | undefined;
  warnings: string[];
  error: { message: string; recovery?: string | undefined } | undefined;
  finished: boolean;
  summary: string | undefined;
}

export function emptyConversationState(): ConversationState {
  return {
    assistantText: "",
    reasoning: [],
    activity: [],
    artifacts: [],
    plan: undefined,
    executionAfterPlan: false,
    pendingPermission: undefined,
    warnings: [],
    error: undefined,
    finished: false,
    summary: undefined,
  };
}

/**
 * Execution evidence: events that show the task moved past its plan — a
 * tool ran (or was asked to), a file appeared, or the task ended. Message
 * text and reasoning are not evidence; the agent narrates while planning.
 */
export function isExecutionEvent(event: AgentEvent): boolean {
  switch (event.type) {
    case "tool.requested":
    case "tool.started":
    case "tool.progress":
    case "tool.completed":
    case "artifact.created":
    case "artifact.modified":
    case "task.completed":
    case "error":
      return true;
    default:
      return false;
  }
}

/**
 * Fold one event into the conversation state. Pure, so it can be replayed
 * over a task's persisted history to reconstruct exactly what the user saw.
 */
export function applyEvent(state: ConversationState, event: AgentEvent): ConversationState {
  const next = reduceEvent(state, event);
  // Track whether anything executed after the newest plan: each plan event
  // resets the marker, each piece of execution evidence sets it.
  if (event.type === "plan.created" || event.type === "plan.updated") {
    return { ...next, executionAfterPlan: false };
  }
  if (isExecutionEvent(event) && !next.executionAfterPlan) {
    return { ...next, executionAfterPlan: true };
  }
  return next;
}

function reduceEvent(state: ConversationState, event: AgentEvent): ConversationState {
  switch (event.type) {
    case "message.started":
      return state;

    case "message.delta":
      return { ...state, assistantText: state.assistantText + event.text };

    case "reasoning.summary":
      return { ...state, reasoning: [...state.reasoning, event.text] };

    case "plan.created":
    case "plan.updated":
      return { ...state, plan: event.plan };

    case "tool.requested":
      // The request itself is not shown: it becomes visible when it starts,
      // or as a permission prompt. Showing both would double every step.
      return state;

    case "tool.started":
      return {
        ...state,
        activity: [
          ...state.activity,
          {
            id: event.call_id,
            title: event.title,
            detail: event.detail ?? undefined,
            status: "running",
          },
        ],
      };

    case "tool.progress":
      return {
        ...state,
        activity: state.activity.map((item) =>
          item.id === event.call_id ? { ...item, detail: event.detail } : item,
        ),
      };

    case "tool.completed": {
      const status: ActivityStatus =
        event.status === "succeeded"
          ? "done"
          : event.status === "denied"
            ? "denied"
            : event.status === "cancelled"
              ? "info"
              : "failed";
      const existing = state.activity.some((item) => item.id === event.call_id);
      const activity = existing
        ? state.activity.map((item) =>
            item.id === event.call_id
              ? { ...item, status, detail: event.summary ?? item.detail }
              : item,
          )
        : [
            ...state.activity,
            {
              id: event.call_id,
              title: event.summary ?? "Finished a step",
              status,
            },
          ];
      return { ...state, activity };
    }

    case "permission.requested":
      return { ...state, pendingPermission: event.request };

    case "artifact.created":
    case "artifact.modified":
      return {
        ...state,
        artifacts: [
          ...state.artifacts.filter((a) => a.id !== event.artifact.id),
          event.artifact,
        ],
      };

    case "warning":
      return { ...state, warnings: [...state.warnings, event.message] };

    case "error":
      return {
        ...state,
        error: { message: event.error.message, recovery: event.error.recovery ?? undefined },
        finished: true,
      };

    case "task.completed":
      return { ...state, finished: true, summary: event.summary };

    default:
      // Exhaustiveness: if a new event type is added without a case above,
      // this line stops compiling.
      return assertHandled(event, state);
  }
}

function assertHandled(_event: never, state: ConversationState): ConversationState {
  return state;
}

/** What a person needs to know while a task is running, in plain words. */
export interface CalmProgress {
  /** One sentence for what is happening right now. */
  headline: string;
  /** "3 of 7 steps done", or undefined when there is nothing to count. */
  progress?: string | undefined;
  /**
   * Steps that failed or were refused. These stay visible: a person must not
   * have to open Details to learn something was not allowed.
   */
  notices: string[];
}

/**
 * Enough notices to show a pattern, few enough to stay a glance. Older ones
 * fall off the top; the timeline under Details keeps the full record.
 */
const MAX_NOTICES = 3;

/** A refused step is not a failure, and the wording says so. */
function noticeText(item: ActivityItem): string {
  return item.status === "denied" ? `Skipped, not allowed: ${item.title}` : item.title;
}

/**
 * The calm view of a running task: one sentence, a count, and anything that
 * went wrong. Everything machine-facing stays in `activity` for the Details
 * disclosure to render.
 */
export function calmProgress(state: ConversationState, running: boolean): CalmProgress {
  const inFlight = [...state.activity].reverse().find((item) => item.status === "running");
  const headline = inFlight
    ? inFlight.title
    : running
      ? "Working…"
      : state.finished
        ? "Finished."
        : "Stopped.";

  const total = state.activity.length;
  const done = state.activity.filter((item) => item.status !== "running").length;

  const notices: string[] = [];
  for (const item of state.activity) {
    if (item.status !== "failed" && item.status !== "denied") continue;
    const text = noticeText(item);
    // The same step can fail more than once (a retry, a repeated refusal);
    // saying so twice adds noise, not information.
    if (!notices.includes(text)) notices.push(text);
  }

  return {
    headline,
    progress:
      total === 0 ? undefined : `${done} of ${total} ${total === 1 ? "step" : "steps"} done`,
    notices: notices.slice(-MAX_NOTICES),
  };
}

/** Plain-language sentence describing a permission request's operation. */
export function permissionHeadline(request: PermissionRequest): string {
  const count = request.paths.length;
  const noun = count === 1 ? "1 item" : `${count} items`;
  switch (request.operation) {
    case "delete":
      return `Move ${noun} to the trash?`;
    case "modify":
      return `Change ${noun}?`;
    case "rename":
      return `Rename ${noun}?`;
    case "move":
      return `Move ${noun} to another folder?`;
    case "create":
      return `Create ${noun}?`;
    case "read":
      return `Read ${noun} outside this project?`;
    case "execute":
      return "Run a program?";
    case "install":
      return "Install software?";
    case "upload":
      return `Upload ${noun}?`;
    case "send":
      return "Send this message?";
    case "publish":
      return "Make this change outside your computer?";
    case "network_fetch":
      return "Fetch something from the internet?";
    case "secret":
      return "Access credentials?";
    default:
      return "Allow this action?";
  }
}
