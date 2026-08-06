import { describe, expect, it } from "vitest";
import {
  calmProgress,
  emptyConversationState,
  type ActivityItem,
  type ActivityStatus,
  type ConversationState,
} from "./activity";

function item(id: string, title: string, status: ActivityStatus): ActivityItem {
  return { id, title, status };
}

function stateWith(activity: ActivityItem[], extra: Partial<ConversationState> = {}) {
  return { ...emptyConversationState(), activity, ...extra };
}

describe("calmProgress", () => {
  it("says nothing is counted before the first step arrives", () => {
    const progress = calmProgress(emptyConversationState(), true);
    expect(progress).toEqual({ headline: "Working…", progress: undefined, notices: [] });
  });

  it("names the step that is happening right now", () => {
    const progress = calmProgress(
      stateWith([
        item("a", "Reading 12 documents", "done"),
        item("b", "Writing the summary", "running"),
      ]),
      true,
    );
    expect(progress.headline).toBe("Writing the summary");
    expect(progress.progress).toBe("1 of 2 steps done");
  });

  it("prefers the newest running step when several are in flight", () => {
    const progress = calmProgress(
      stateWith([item("a", "Searching", "running"), item("b", "Reading a file", "running")]),
      true,
    );
    expect(progress.headline).toBe("Reading a file");
    expect(progress.progress).toBe("0 of 2 steps done");
  });

  it("falls back to a plain sentence when every step has ended but work continues", () => {
    const progress = calmProgress(stateWith([item("a", "Reading a file", "done")]), true);
    expect(progress.headline).toBe("Working…");
    expect(progress.progress).toBe("1 of 1 step done");
  });

  it("says the task finished", () => {
    const progress = calmProgress(
      stateWith([item("a", "Reading a file", "done")], { finished: true }),
      false,
    );
    expect(progress.headline).toBe("Finished.");
  });

  it("says the task stopped when it neither runs nor finished", () => {
    const progress = calmProgress(stateWith([item("a", "Reading a file", "done")]), false);
    expect(progress.headline).toBe("Stopped.");
  });

  it("counts a step still running as not done", () => {
    const progress = calmProgress(stateWith([item("a", "Searching", "running")]), true);
    expect(progress.progress).toBe("0 of 1 step done");
  });

  it("shows failures and refusals, wording a refusal as a skip", () => {
    const progress = calmProgress(
      stateWith([
        item("a", "Reading 12 documents", "done"),
        item("b", "Saving the report", "failed"),
        item("c", "Deleting the old draft", "denied"),
      ]),
      false,
    );
    expect(progress.notices).toEqual([
      "Saving the report",
      "Skipped, not allowed: Deleting the old draft",
    ]);
  });

  it("says the same thing once, however often it happened", () => {
    const progress = calmProgress(
      stateWith([
        item("a", "Saving the report", "failed"),
        item("b", "Saving the report", "failed"),
        item("c", "Deleting the old draft", "denied"),
        item("d", "Deleting the old draft", "denied"),
      ]),
      false,
    );
    expect(progress.notices).toEqual([
      "Saving the report",
      "Skipped, not allowed: Deleting the old draft",
    ]);
  });

  it("keeps the three newest notices", () => {
    const progress = calmProgress(
      stateWith([
        item("a", "First", "failed"),
        item("b", "Second", "failed"),
        item("c", "Third", "denied"),
        item("d", "Fourth", "failed"),
      ]),
      false,
    );
    expect(progress.notices).toEqual(["Second", "Skipped, not allowed: Third", "Fourth"]);
  });

  it("leaves notices empty when nothing went wrong", () => {
    const progress = calmProgress(
      stateWith([item("a", "Reading a file", "done"), item("b", "A note", "info")]),
      true,
    );
    expect(progress.notices).toEqual([]);
  });
});
