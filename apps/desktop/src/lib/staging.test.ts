/**
 * The Artifact Studio's rules, tested as pure data transforms (no DOM, no
 * backend): how proposals are grouped and ordered, how selection behaves
 * around a conflicted change, and the exact words the screen puts in front of
 * someone deciding whether to let an agent touch their documents.
 */
import { describe, expect, it } from "vitest";
import type { OperationResult } from "@commonspace/protocol";
import type { ChangePreview, StagedChange } from "./ipc";
import {
  allApplicableSelected,
  applicableIds,
  applyLabel,
  basisNote,
  conflictWarning,
  describeDiffTotals,
  destinationLine,
  diffShapeRows,
  discardConfirmation,
  discardLabel,
  fileNameOf,
  groupChanges,
  hunkRange,
  kindGlyph,
  kindLabel,
  kindTone,
  missingDiffReason,
  noDifferenceNote,
  orderedChanges,
  pendingNotice,
  pendingSummary,
  pruneSelection,
  selectAll,
  selectNone,
  selectedChanges,
  sizeLine,
  summarizeApplied,
  summarizeDiscarded,
  toggleSelection,
} from "./staging";

/* --------------------------------------------------------------- fixtures */

function change(overrides: Partial<StagedChange> & { id: string }): StagedChange {
  return {
    task_id: "task_1",
    kind: "modify",
    target: `C:/ws/${overrides.id}.docx`,
    destination: null,
    summary: `Update ${overrides.id}.docx`,
    size_after: null,
    conflicted: false,
    staged_at: "2026-08-07T09:00:00Z",
    ...overrides,
  };
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
    user_summary: success ? "Applied." : "Could not apply.",
    diagnostics: null,
  };
}

function summary(
  overrides: Partial<NonNullable<ChangePreview["summary"]>> = {},
): NonNullable<ChangePreview["summary"]> {
  return { reason: "too_large", old_bytes: 1_200_000, new_bytes: 1_300_000, ...overrides };
}

/* ------------------------------------------------------------------ kinds */

describe("change kinds", () => {
  it("names every kind in plain language", () => {
    expect(kindLabel("create")).toBe("New file");
    expect(kindLabel("modify")).toBe("Edit");
    expect(kindLabel("rename")).toBe("Rename");
    expect(kindLabel("move")).toBe("Move");
    expect(kindLabel("delete")).toBe("Delete");
  });

  it("gives every kind a glyph, so tone is never the only signal", () => {
    for (const kind of ["create", "modify", "rename", "move", "delete"] as const) {
      expect(kindGlyph(kind).length).toBeGreaterThan(0);
    }
  });

  it("reserves the danger tone for deletion", () => {
    expect(kindTone("delete")).toBe("danger");
    expect(kindTone("create")).toBe("ok");
    expect(kindTone("modify")).toBe("warn");
    expect(kindTone("move")).toBe("warn");
  });
});

describe("fileNameOf", () => {
  it("reads the last segment of either platform's separator", () => {
    expect(fileNameOf("C:/ws/reports/Q3.docx")).toBe("Q3.docx");
    expect(fileNameOf("C:\\ws\\reports\\Q3.docx")).toBe("Q3.docx");
    expect(fileNameOf("Q3.docx")).toBe("Q3.docx");
  });

  it("survives a trailing separator rather than returning nothing", () => {
    expect(fileNameOf("C:/ws/reports/")).toBe("reports");
  });
});

describe("destinationLine", () => {
  it("names the new file for a rename and the full path for a move", () => {
    expect(
      destinationLine(
        change({ id: "a", kind: "rename", destination: "C:/ws/2024-03-14 scan.pdf" }),
      ),
    ).toBe("Renaming to 2024-03-14 scan.pdf");
    expect(
      destinationLine(change({ id: "a", kind: "move", destination: "C:/ws/Archive/a.docx" })),
    ).toBe("Moving to C:/ws/Archive/a.docx");
  });

  it("says nothing for a kind that has no destination", () => {
    expect(destinationLine(change({ id: "a", kind: "modify" }))).toBeUndefined();
  });
});

describe("sizeLine", () => {
  it("reports the size the file would end up at", () => {
    expect(sizeLine(change({ id: "a", size_after: 12_000 }), "en-US")).toBe("12 kB afterwards");
  });

  it("omits the size for a deletion and when the backend did not report one", () => {
    expect(sizeLine(change({ id: "a", kind: "delete", size_after: 12_000 }))).toBeUndefined();
    expect(sizeLine(change({ id: "a", size_after: null }))).toBeUndefined();
  });
});

/* --------------------------------------------------------------- grouping */

describe("groupChanges", () => {
  it("puts conflicts first, then orders groups by consequence", () => {
    const groups = groupChanges([
      change({ id: "new", kind: "create" }),
      change({ id: "edit", kind: "modify" }),
      change({ id: "moved", kind: "move", destination: "C:/ws/Archive/moved.docx" }),
      change({ id: "gone", kind: "delete" }),
      change({ id: "clash", kind: "modify", conflicted: true }),
    ]);
    expect(groups.map((group) => group.id)).toEqual([
      "conflicted",
      "delete",
      "relocate",
      "modify",
      "create",
    ]);
  });

  it("takes a conflicted change out of its kind group entirely", () => {
    const groups = groupChanges([
      change({ id: "clean", kind: "delete" }),
      change({ id: "clash", kind: "delete", conflicted: true }),
    ]);
    expect(groups[0]?.id).toBe("conflicted");
    expect(groups[0]?.changes.map((c) => c.id)).toEqual(["clash"]);
    expect(groups[1]?.changes.map((c) => c.id)).toEqual(["clean"]);
  });

  it("explains the conflicted group and leaves the ordinary ones unannotated", () => {
    const groups = groupChanges([
      change({ id: "clash", conflicted: true }),
      change({ id: "edit" }),
    ]);
    expect(groups[0]?.note).toContain("changed on disk");
    expect(groups[1]?.note).toBeUndefined();
  });

  it("drops empty groups", () => {
    expect(groupChanges([change({ id: "only", kind: "create" })]).map((g) => g.id)).toEqual([
      "create",
    ]);
  });

  it("orders within a group by the file affected, so the list is stable", () => {
    const groups = groupChanges([
      change({ id: "b", target: "C:/ws/zebra.docx" }),
      change({ id: "a", target: "C:/ws/apple.docx" }),
    ]);
    expect(groups[0]?.changes.map((c) => c.target)).toEqual([
      "C:/ws/apple.docx",
      "C:/ws/zebra.docx",
    ]);
  });

  it("returns nothing for an empty staging area", () => {
    expect(groupChanges([])).toEqual([]);
  });
});

describe("orderedChanges", () => {
  it("flattens the groups into the order they are reviewed in", () => {
    const ordered = orderedChanges([
      change({ id: "new", kind: "create" }),
      change({ id: "clash", conflicted: true }),
      change({ id: "gone", kind: "delete" }),
    ]);
    expect(ordered.map((c) => c.id)).toEqual(["clash", "gone", "new"]);
  });
});

/* -------------------------------------------------------------- selection */

describe("selection", () => {
  const changes = [
    change({ id: "a" }),
    change({ id: "b" }),
    change({ id: "clash", conflicted: true }),
  ];

  it("never counts a conflicted change as applicable", () => {
    expect(applicableIds(changes)).toEqual(["a", "b"]);
  });

  it("selects everything except conflicts by default", () => {
    expect([...selectAll(changes)]).toEqual(["a", "b"]);
    expect([...selectNone()]).toEqual([]);
  });

  it("toggles one id at a time in both directions", () => {
    const once = toggleSelection(new Set(["a"]), "b");
    expect([...once].sort()).toEqual(["a", "b"]);
    expect([...toggleSelection(once, "a")]).toEqual(["b"]);
  });

  it("lets a conflicted change be selected deliberately", () => {
    expect(toggleSelection(selectAll(changes), "clash").has("clash")).toBe(true);
  });

  it("knows when every applicable change is selected, ignoring conflicts", () => {
    expect(allApplicableSelected(changes, new Set(["a", "b"]))).toBe(true);
    expect(allApplicableSelected(changes, new Set(["a"]))).toBe(false);
    expect(allApplicableSelected([change({ id: "clash", conflicted: true })], new Set())).toBe(
      false,
    );
  });

  it("forgets ids that are no longer staged", () => {
    expect([...pruneSelection(new Set(["a", "gone"]), changes)]).toEqual(["a"]);
  });

  it("returns the selected changes in review order", () => {
    const selected = selectedChanges(
      [change({ id: "new", kind: "create" }), change({ id: "gone", kind: "delete" })],
      new Set(["new", "gone"]),
    );
    expect(selected.map((c) => c.id)).toEqual(["gone", "new"]);
  });
});

/* ---------------------------------------------------------------- wording */

describe("pendingSummary", () => {
  it("says how many files change and how many need attention", () => {
    expect(
      pendingSummary([
        change({ id: "a", target: "C:/ws/a.docx" }),
        change({ id: "b", target: "C:/ws/b.docx" }),
        change({ id: "c", target: "C:/ws/c.docx", conflicted: true }),
      ]),
    ).toBe("3 files will change, 1 needs your attention.");
  });

  it("counts files, not changes, when one file is touched twice", () => {
    expect(
      pendingSummary([
        change({ id: "a", target: "C:/ws/a.docx" }),
        change({ id: "b", target: "C:/ws/a.docx", kind: "rename" }),
      ]),
    ).toBe("1 file will change.");
  });

  it("agrees its verb when several files need attention", () => {
    expect(
      pendingSummary([
        change({ id: "a", target: "C:/ws/a.docx", conflicted: true }),
        change({ id: "b", target: "C:/ws/b.docx", conflicted: true }),
      ]),
    ).toBe("2 files will change, 2 need your attention.");
  });

  it("is plain about an empty staging area", () => {
    expect(pendingSummary([])).toBe("Nothing is waiting for your review.");
  });
});

describe("pendingNotice", () => {
  it("counts what is waiting, for the panel that points at the Studio", () => {
    expect(pendingNotice(1)).toBe("1 change is waiting for your review.");
    expect(pendingNotice(4)).toBe("4 changes are waiting for your review.");
  });

  it("says nothing when nothing is staged", () => {
    expect(pendingNotice(0)).toBeUndefined();
  });
});

describe("button labels", () => {
  it("puts the count inside the label, where it is read before the press", () => {
    expect(applyLabel(3)).toBe("Apply 3 changes");
    expect(applyLabel(1)).toBe("Apply 1 change");
    expect(discardLabel(2)).toBe("Discard 2 changes");
    expect(discardLabel(1)).toBe("Discard 1 change");
  });

  it("drops the count when there is nothing selected", () => {
    expect(applyLabel(0)).toBe("Apply changes");
    expect(discardLabel(0)).toBe("Discard changes");
  });
});

describe("conflictWarning", () => {
  it("spells out what applying a conflicted change costs", () => {
    expect(conflictWarning(1)).toContain("will overwrite that change");
    expect(conflictWarning(2)).toContain("2 changes you selected");
  });

  it("stays silent when no conflicted change is selected", () => {
    expect(conflictWarning(0)).toBeUndefined();
  });
});

describe("discardConfirmation", () => {
  it("says what survives, so discard is never mistaken for delete", () => {
    expect(discardConfirmation(1)).toContain("your file stays as it is");
    expect(discardConfirmation(4)).toContain("Discard 4 changes?");
  });
});

describe("summarizeApplied", () => {
  it("reports a clean run", () => {
    expect(summarizeApplied([opResult(true), opResult(true)])).toBe(
      "2 changes applied to your files.",
    );
    expect(summarizeApplied([opResult(true)])).toBe("1 change applied to your files.");
  });

  it("does not report success over a partial failure", () => {
    expect(summarizeApplied([opResult(true), opResult(false), opResult(true)])).toBe(
      "2 of 3 changes applied. The rest could not be, and those files were left as they are.",
    );
  });

  it("is explicit when nothing landed", () => {
    expect(summarizeApplied([opResult(false)])).toContain("could not be applied");
    expect(summarizeApplied([opResult(false), opResult(false)])).toContain("None of these");
    expect(summarizeApplied([])).toBe("Nothing was applied.");
  });
});

describe("summarizeDiscarded", () => {
  it("reassures that discarding touched nothing", () => {
    expect(summarizeDiscarded(2)).toBe("2 changes discarded. Your files were not touched.");
    expect(summarizeDiscarded(1)).toBe("1 change discarded. Your files were not touched.");
    expect(summarizeDiscarded(0)).toBe("Nothing was discarded.");
  });
});

/* ------------------------------------------------------------------ diffs */

describe("describeDiffTotals", () => {
  it("counts both directions", () => {
    expect(describeDiffTotals(12, 3, "en-US")).toBe("12 lines added, 3 removed.");
    expect(describeDiffTotals(1, 0, "en-US")).toBe("1 line added.");
    expect(describeDiffTotals(0, 5, "en-US")).toBe("5 lines removed.");
  });

  it("says so when nothing moved", () => {
    expect(describeDiffTotals(0, 0)).toBe("No lines added or removed.");
  });
});

describe("missingDiffReason", () => {
  it("explains both reasons a diff can be missing", () => {
    expect(missingDiffReason("too_large")).toContain("too large");
    expect(missingDiffReason("not_text")).toContain("not text");
  });
});

describe("diffShapeRows", () => {
  it("shows before and after in sizes and lines", () => {
    expect(diffShapeRows(summary({ old_lines: 4102, new_lines: 4180 }), "en-US")).toEqual([
      { label: "Before", value: "1.2 MB, 4,102 lines" },
      { label: "After", value: "1.3 MB, 4,180 lines" },
    ]);
  });

  it("falls back to sizes alone when the file has no line count", () => {
    expect(diffShapeRows(summary({ reason: "not_text" }), "en-US")).toEqual([
      { label: "Before", value: "1.2 MB" },
      { label: "After", value: "1.3 MB" },
    ]);
  });
});

describe("noDifferenceNote", () => {
  it("is precise about what was compared", () => {
    expect(noDifferenceNote("extracted_text")).toBe(
      "The text Commonspace can read from this file is unchanged.",
    );
    expect(noDifferenceNote("full_text")).toBe("The text in this file is unchanged.");
  });
});

describe("basisNote", () => {
  it("explains an extracted-text comparison when nothing else does", () => {
    expect(basisNote("extracted_text", null)).toContain("extracted");
  });

  it("stands aside for the backend's own caveat", () => {
    expect(basisNote("extracted_text", "Formatting changes are not shown.")).toBeUndefined();
  });

  it("says nothing about an ordinary text comparison", () => {
    expect(basisNote("full_text", null)).toBeUndefined();
  });
});

describe("hunkRange", () => {
  it("names the stretch of the file a hunk covers", () => {
    expect(hunkRange({ new_start: 12, new_lines: 7 }, "en-US")).toBe("Lines 12–18");
    expect(hunkRange({ new_start: 12, new_lines: 1 }, "en-US")).toBe("Line 12");
  });

  it("degrades to a single line when a hunk spans none", () => {
    expect(hunkRange({ new_start: 40, new_lines: 0 }, "en-US")).toBe("Line 40");
  });
});
