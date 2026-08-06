/**
 * The replay rules, tested as pure data transforms (no DOM, no backend):
 * folding a persisted event stream back into the conversation state,
 * suppressing assistant text the orchestrator already persisted to
 * `messages`, aggregating artifacts across a conversation's tasks, and
 * deriving the outcome card for every terminal shape a task can have.
 */
import { describe, expect, it } from "vitest";
import {
  agentEventSchema,
  type AgentEvent,
  type Artifact,
  type Message,
  type OperationResult,
  type TaskState,
} from "@commonspace/protocol";
import {
  aggregateArtifacts,
  deriveOutcome,
  foldEvents,
  isInterruptedTask,
  newestTask,
  replayConversationState,
  summarizeUndo,
  type ReplayTask,
} from "./replay";

/* --------------------------------------------------------------- fixtures */

function artifact(
  overrides: Partial<Artifact> & { id: string; task_id: string },
): Artifact {
  return {
    kind: "docx",
    path: `C:/ws/${overrides.id}.docx`,
    name: `${overrides.id}.docx`,
    modified_existing: false,
    backup_path: null,
    file_operation_id: null,
    change_summary: null,
    created_at: "2026-08-05T12:00:00Z",
    ...overrides,
  };
}

function task(state: TaskState, overrides: Partial<ReplayTask> = {}): ReplayTask {
  return { id: "task_1", state, summary: null, error_message: null, ...overrides };
}

function opResult(success: boolean): OperationResult {
  return {
    success,
    created: [],
    modified: [],
    backups: [],
    warnings: [],
    validation: success
      ? { outcome: "passed" }
      : { outcome: "failed", detail: "The file changed since." },
    user_summary: success ? "Restored the file." : "Couldn't restore the file.",
  };
}

const summaryArtifact = artifact({
  id: "art_1",
  task_id: "task_1",
  file_operation_id: "fop_1",
  change_summary: "Created from 12 contracts",
});

/**
 * A realistic completed-task stream, shaped like the Rust-generated
 * fixtures in `tests/fixtures/protocol-samples.json` and validated below
 * against the real event schema so the fixtures cannot drift.
 */
const completedRun: AgentEvent[] = [
  { type: "message.started", message_id: "msg_1", role: "assistant" },
  { type: "message.delta", message_id: "msg_1", text: "I compared the contracts " },
  { type: "message.delta", message_id: "msg_1", text: "and wrote a summary." },
  { type: "reasoning.summary", text: "Considering the folder" },
  {
    type: "plan.created",
    plan: {
      steps: [{ title: "Read 12 documents", detail: "Contracts folder" }],
      paths_accessed: ["C:/ws/contracts"],
      paths_likely_modified: ["C:/ws/summary.docx"],
      external_services: [],
      consequential_actions: ["Create summary.docx"],
      deliverables: ["A summary document"],
      requires_approval: true,
    },
  },
  {
    type: "tool.requested",
    call_id: "tool_1",
    tool: "Read",
    title: "Reading contracts",
    paths: ["C:/ws/contracts"],
  },
  { type: "tool.started", call_id: "tool_1", title: "Reading 12 documents" },
  { type: "tool.progress", call_id: "tool_1", detail: "8 of 12" },
  { type: "tool.completed", call_id: "tool_1", status: "succeeded", summary: "Read 12 documents" },
  { type: "artifact.created", artifact: summaryArtifact },
  { type: "task.completed", summary: "Compared 12 contracts and wrote summary.docx." },
];

/* ------------------------------------------------------------ event fold */

describe("foldEvents", () => {
  it("uses shapes the real event schema accepts", () => {
    for (const event of completedRun) {
      expect(agentEventSchema.safeParse(event).success, event.type).toBe(true);
    }
  });

  it("rebuilds the state a completed task showed live", () => {
    const state = foldEvents(completedRun);
    expect(state.assistantText).toBe("I compared the contracts and wrote a summary.");
    expect(state.reasoning).toEqual(["Considering the folder"]);
    expect(state.plan?.steps[0]?.title).toBe("Read 12 documents");
    expect(state.activity).toHaveLength(1);
    expect(state.activity[0]).toMatchObject({
      id: "tool_1",
      status: "done",
      detail: "Read 12 documents",
    });
    expect(state.artifacts.map((a) => a.id)).toEqual(["art_1"]);
    expect(state.finished).toBe(true);
    expect(state.summary).toBe("Compared 12 contracts and wrote summary.docx.");
  });
});

describe("replayConversationState", () => {
  const message = (role: Message["role"], content: string): Message => ({
    id: `m-${role}-${content.length}`,
    conversation_id: "conv_1",
    role,
    content,
    created_at: "2026-08-05T12:01:00Z",
  });

  it("suppresses assistant text the orchestrator persisted to messages", () => {
    // For any task whose stream ended in a live process, orchestrator.rs
    // appends the concatenated deltas as an assistant message — the stored
    // message is the durable record, so the replayed copy must not render.
    const messages = [
      message("user", "Compare these contracts"),
      message("assistant", "I compared the contracts and wrote a summary."),
    ];
    const state = replayConversationState(completedRun, messages);
    expect(state.assistantText).toBe("");
    // Everything else survives: events still drive timeline and artifacts.
    expect(state.activity).toHaveLength(1);
    expect(state.artifacts).toHaveLength(1);
  });

  it("keeps assistant text when no matching message exists (crash before persist)", () => {
    const state = replayConversationState(completedRun, [
      message("user", "Compare these contracts"),
    ]);
    expect(state.assistantText).toBe("I compared the contracts and wrote a summary.");
  });

  it("never resurrects a permission prompt as answerable", () => {
    const events: AgentEvent[] = [
      {
        type: "permission.requested",
        request: {
          id: "perm_1",
          task_id: "task_1",
          operation: "delete",
          summary: "Move 3 duplicate files to the trash",
          paths: ["C:/ws/copy of a.txt"],
          items: [],
          risk: "high",
          irreversible: false,
          requested_at: "2026-08-05T12:00:00Z",
        },
      },
    ];
    expect(foldEvents(events).pendingPermission).toBeDefined();
    expect(replayConversationState(events, []).pendingPermission).toBeUndefined();
  });
});

/* ------------------------------------------------------------ aggregation */

describe("aggregateArtifacts", () => {
  it("merges tasks oldest-first, dropping duplicate ids", () => {
    const a = artifact({ id: "art_a", task_id: "t1" });
    const b = artifact({ id: "art_b", task_id: "t2" });
    const merged = aggregateArtifacts([
      { taskId: "t1", artifacts: [a] },
      { taskId: "t2", artifacts: [a, b] },
    ]);
    expect(merged.map((x) => x.id)).toEqual(["art_a", "art_b"]);
  });

  it("keeps the newest artifact when a later task touches the same path", () => {
    const created = artifact({ id: "art_old", task_id: "t1", path: "C:/ws/report.docx" });
    const modified = artifact({
      id: "art_new",
      task_id: "t2",
      path: "C:/ws/report.docx",
      modified_existing: true,
      file_operation_id: "fop_2",
    });
    const untouched = artifact({ id: "art_other", task_id: "t1" });
    const merged = aggregateArtifacts([
      { taskId: "t1", artifacts: [created, untouched] },
      { taskId: "t2", artifacts: [modified] },
    ]);
    expect(merged.map((x) => x.id)).toEqual(["art_other", "art_new"]);
    // Per-task grouping is preserved in the data itself: the artifact still
    // names the task that made it, which is the undo-task scope.
    expect(merged[1]?.task_id).toBe("t2");
  });
});

/* ---------------------------------------------------------- deriveOutcome */

describe("deriveOutcome", () => {
  it("completed: summary, files, and a task-level undo", () => {
    const state = foldEvents(completedRun);
    const outcome = deriveOutcome(task("completed"), state, [summaryArtifact]);
    expect(outcome.kind).toBe("completed");
    expect(outcome.headline).toBe("What Commonspace did");
    expect(outcome.summary).toBe("Compared 12 contracts and wrote summary.docx.");
    expect(outcome.artifacts.map((a) => a.id)).toEqual(["art_1"]);
    expect(outcome.canUndoTask).toBe(true);
    expect(outcome.note).toBeUndefined();
    expect(outcome.error).toBeUndefined();
    expect(outcome.deniedNote).toBeUndefined();
  });

  it("completed with nothing produced says so plainly", () => {
    const state = foldEvents([{ type: "task.completed", summary: "Looked around." }]);
    const outcome = deriveOutcome(task("completed"), state, []);
    expect(outcome.kind).toBe("completed");
    expect(outcome.note).toBe("No files were changed.");
    expect(outcome.canUndoTask).toBe(false);
  });

  it("failed: the error and its recovery hint, plainly", () => {
    const events: AgentEvent[] = [
      {
        type: "error",
        error: {
          code: "provider_exit",
          message: "The agent stopped unexpectedly.",
          recovery: "Try sending the request again.",
          transient: true,
        },
      },
    ];
    const outcome = deriveOutcome(
      task("failed", { error_message: "The agent stopped unexpectedly." }),
      foldEvents(events),
      [],
    );
    expect(outcome.kind).toBe("failed");
    expect(outcome.headline).toBe("This task didn't finish.");
    expect(outcome.error).toEqual({
      message: "The agent stopped unexpectedly.",
      recovery: "Try sending the request again.",
    });
    expect(outcome.summary).toBeUndefined();
  });

  it("cancelled: you stopped this task", () => {
    const outcome = deriveOutcome(task("cancelled"), foldEvents([]), []);
    expect(outcome.kind).toBe("cancelled");
    expect(outcome.headline).toBe("You stopped this task.");
  });

  it("permission denied: noted calmly on the card", () => {
    const events: AgentEvent[] = [
      { type: "tool.started", call_id: "tool_1", title: "Deleting duplicates" },
      { type: "tool.completed", call_id: "tool_1", status: "denied", summary: "Not allowed" },
      { type: "task.completed", summary: "Finished without deleting anything." },
    ];
    const outcome = deriveOutcome(task("completed"), foldEvents(events), []);
    expect(outcome.kind).toBe("completed");
    expect(outcome.deniedNote).toBe("You declined a change, so Commonspace stopped there.");
  });

  it("interrupted: a task still marked running renders honestly", () => {
    const outcome = deriveOutcome(task("running"), foldEvents([]), []);
    expect(outcome.kind).toBe("interrupted");
    expect(outcome.headline).toBe("This task was interrupted.");
    expect(outcome.note).toContain("Send a new message to continue the work.");
  });

  it("interrupted: crash recovery's failed-with-explanation is not a plain failure", () => {
    // Storage::fail_task_for_recovery stores exactly the explanation
    // Orchestrator::recover_after_restart passes it.
    const crashMessage =
      "Commonspace closed while this task was running, so it was stopped. " +
      "Any changes already made are listed below and can be undone.";
    const crashed = task("failed", { error_message: crashMessage });
    expect(isInterruptedTask(crashed)).toBe(true);
    const changed = artifact({ id: "art_1", task_id: "task_1", file_operation_id: "fop_1" });
    const outcome = deriveOutcome(crashed, foldEvents([]), [changed]);
    expect(outcome.kind).toBe("interrupted");
    expect(outcome.note).toContain(crashMessage);
    // The changes it already made are shown and undoable, as the copy says.
    expect(outcome.artifacts).toHaveLength(1);
    expect(outcome.canUndoTask).toBe(true);
  });

  it("scopes the card to the task's own artifacts", () => {
    const own = artifact({ id: "art_mine", task_id: "task_1" });
    const other = artifact({ id: "art_other", task_id: "task_2" });
    const outcome = deriveOutcome(task("completed"), foldEvents([]), [own, other]);
    expect(outcome.artifacts.map((a) => a.id)).toEqual(["art_mine"]);
  });
});

/* ---------------------------------------------------------------- helpers */

describe("newestTask", () => {
  it("picks the latest created_at regardless of order", () => {
    const tasks = [
      { id: "a", created_at: "2026-08-05T10:00:00Z" },
      { id: "c", created_at: "2026-08-06T09:00:00Z" },
      { id: "b", created_at: "2026-08-05T12:00:00Z" },
    ];
    expect(newestTask(tasks)?.id).toBe("c");
    expect(newestTask([])).toBeUndefined();
  });
});

describe("summarizeUndo", () => {
  it("counts clean undos", () => {
    expect(summarizeUndo([opResult(true)])).toBe("1 change undone.");
    expect(summarizeUndo([opResult(true), opResult(true), opResult(true)])).toBe(
      "3 changes undone.",
    );
  });

  it("is honest about partial failure", () => {
    expect(summarizeUndo([opResult(true), opResult(false), opResult(true)])).toBe(
      "2 of 3 changes undone. The rest could not be — those files may have changed since.",
    );
  });

  it("is honest about total failure and the empty case", () => {
    expect(summarizeUndo([opResult(false)])).toContain("could not be undone");
    expect(summarizeUndo([])).toBe("There was nothing to undo.");
  });
});
