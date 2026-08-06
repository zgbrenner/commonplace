import { describe, expect, it } from "vitest";
import { completionNotification, notifyTaskFinished, NOTIFICATIONS_SETTING } from "./notify";

describe("completionNotification", () => {
  it("names the project and the files a completed task changed", () => {
    expect(
      completionNotification({
        projectName: "Tax 2024",
        state: "completed",
        summary: "Renamed 37 scans to their scan dates.",
        changedFiles: 37,
      }),
    ).toEqual({
      title: "Commonspace finished in Tax 2024",
      body: "37 files changed. Renamed 37 scans to their scan dates.",
    });
  });

  it("uses the singular for a single changed file", () => {
    expect(
      completionNotification({
        projectName: "Tax 2024",
        state: "completed",
        summary: undefined,
        changedFiles: 1,
      }).body,
    ).toBe("1 file changed.");
  });

  it("says plainly when a completed task changed nothing", () => {
    expect(
      completionNotification({
        projectName: "Tax 2024",
        state: "completed",
        summary: "Everything was already named correctly.",
        changedFiles: 0,
      }),
    ).toEqual({
      title: "Commonspace finished in Tax 2024",
      body: "No files were changed. Everything was already named correctly.",
    });
  });

  it("leads a failure with the outcome, then the reason", () => {
    expect(
      completionNotification({
        projectName: "Invoices",
        state: "failed",
        summary: "The folder could not be read.",
        changedFiles: 0,
      }),
    ).toEqual({
      title: "Commonspace didn't finish in Invoices",
      body: "This task didn't finish. The folder could not be read.",
    });
  });

  it("says what a failed task had already changed", () => {
    expect(
      completionNotification({
        projectName: "Invoices",
        state: "failed",
        summary: "The folder could not be read.",
        changedFiles: 2,
      }).body,
    ).toBe("This task didn't finish. The folder could not be read. 2 files changed before it stopped.");
  });

  it("admits when a failure came with no reason", () => {
    expect(
      completionNotification({
        projectName: undefined,
        state: "failed",
        summary: undefined,
        changedFiles: 0,
      }),
    ).toEqual({
      title: "Commonspace didn't finish",
      body: "This task didn't finish. No reason was reported.",
    });
  });

  it("says that the person stopped a cancelled task", () => {
    expect(
      completionNotification({
        projectName: "Photos",
        state: "cancelled",
        summary: undefined,
        changedFiles: 0,
      }),
    ).toEqual({
      title: "Task stopped in Photos",
      body: "You stopped this task. No files were changed.",
    });
  });

  it("reports what a cancelled task had already changed", () => {
    expect(
      completionNotification({
        projectName: undefined,
        state: "cancelled",
        summary: undefined,
        changedFiles: 1,
      }),
    ).toEqual({
      title: "Task stopped",
      body: "You stopped this task. 1 file changed before it stopped.",
    });
  });

  it("drops the project name when there isn't one", () => {
    for (const projectName of [undefined, "", "   "]) {
      const message = completionNotification({
        projectName,
        state: "completed",
        summary: undefined,
        changedFiles: 0,
      });
      expect(message.title).toBe("Commonspace finished");
      expect(message.title).not.toContain("undefined");
    }
  });

  it("never prints 'undefined' for a missing summary", () => {
    for (const summary of [undefined, "", "   "]) {
      const message = completionNotification({
        projectName: "Photos",
        state: "completed",
        summary,
        changedFiles: 3,
      });
      expect(message.body).toBe("3 files changed.");
    }
  });

  it("shortens a long summary at a word boundary with a visible ellipsis", () => {
    const long = "It renamed every scanned receipt in the folder ".repeat(10);
    const { body } = completionNotification({
      projectName: "Tax 2024",
      state: "completed",
      summary: long,
      changedFiles: 4,
    });
    expect(body.length).toBeLessThan(200);
    expect(body.endsWith("…")).toBe(true);
    expect(body).not.toContain(" …");
    expect(body.startsWith("4 files changed. It renamed")).toBe(true);
  });

  it("leaves a summary that already fits untouched", () => {
    const summary = "Sorted the folder by date.";
    expect(
      completionNotification({
        projectName: "Photos",
        state: "completed",
        summary,
        changedFiles: 2,
      }).body,
    ).toBe(`2 files changed. ${summary}`);
  });

  it("treats a negative or fractional file count as a whole number of files", () => {
    expect(
      completionNotification({
        projectName: undefined,
        state: "completed",
        summary: undefined,
        changedFiles: -1,
      }).body,
    ).toBe("No files were changed.");
  });
});

describe("notifyTaskFinished", () => {
  it("resolves quietly when there is no Tauri host", async () => {
    // The unit test environment has no window and no plugin, which is exactly
    // the failure a running task must survive.
    await expect(
      notifyTaskFinished({ title: "Commonspace finished", body: "No files were changed." }),
    ).resolves.toBeUndefined();
  });
});

describe("NOTIFICATIONS_SETTING", () => {
  it("is the stable key Settings reads and writes", () => {
    expect(NOTIFICATIONS_SETTING).toBe("notifications.enabled");
  });
});
