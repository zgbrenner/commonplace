/**
 * The rules the Artifact Studio reviews proposed changes by.
 *
 * A staged change is a proposal: it lives in Commonspace's staging area and
 * has not touched the user's file yet. Everything that decides how a set of
 * proposals is grouped, ordered, selected and put into words lives here as
 * pure functions over the IPC shapes, so the rules can be tested without a
 * DOM — this app has no DOM testing library, and a review surface whose
 * rules are untestable is a review surface nobody can trust.
 *
 * Wording rules that the whole file follows: counts are explicit (a person
 * about to overwrite five documents should be told it is five), nothing is
 * softened, and a change that cannot be shown says so rather than rendering
 * as an absence.
 */
import type { OperationResult } from "@commonspace/protocol";
import type { ChangePreview, StagedChange } from "./ipc";
import { formatCount, formatFileSize } from "./format";

type ChangeKind = StagedChange["kind"];

/* ------------------------------------------------------------------ kinds */

/** What a change kind is called in the reader's language, not git's. */
export function kindLabel(kind: ChangeKind): string {
  switch (kind) {
    case "create":
      return "New file";
    case "modify":
      return "Edit";
    case "rename":
      return "Rename";
    case "move":
      return "Move";
    case "delete":
      return "Delete";
  }
}

/**
 * The second, non-colour channel for a kind (WCAG 1.4.1). Rename and move
 * share an arrow deliberately: they are the same motion, and the label beside
 * it already says which one.
 */
export function kindGlyph(kind: ChangeKind): string {
  switch (kind) {
    case "create":
      return "+";
    case "modify":
      return "±";
    case "rename":
    case "move":
      return "→";
    case "delete":
      return "✕";
  }
}

/** Pill tone per kind, ordered by how much of the user's work is at stake. */
export function kindTone(kind: ChangeKind): "ok" | "warn" | "danger" {
  switch (kind) {
    case "create":
      return "ok";
    case "modify":
    case "rename":
    case "move":
      return "warn";
    case "delete":
      return "danger";
  }
}

/* -------------------------------------------------------------- filenames */

/** The last segment of a path, whichever separator the platform used. */
export function fileNameOf(path: string): string {
  const segments = path.split(/[/\\]/).filter((segment) => segment.length > 0);
  return segments.length > 0 ? (segments[segments.length - 1] as string) : path;
}

/** Where a rename or move would put the file; nothing for other kinds. */
export function destinationLine(change: StagedChange): string | undefined {
  if (!change.destination) return undefined;
  if (change.kind === "rename") return `Renaming to ${fileNameOf(change.destination)}`;
  if (change.kind === "move") return `Moving to ${change.destination}`;
  return undefined;
}

/**
 * How big the file would be afterwards. Omitted for a deletion, where the
 * answer is "gone" and a byte count would only be confusing.
 */
export function sizeLine(change: StagedChange, locale?: string | undefined): string | undefined {
  if (change.kind === "delete") return undefined;
  if (change.size_after === null || change.size_after === undefined) return undefined;
  const size = formatFileSize(change.size_after, locale);
  return size ? `${size} afterwards` : undefined;
}

/* --------------------------------------------------------------- grouping */

export type ChangeGroupId = "conflicted" | "delete" | "relocate" | "modify" | "create";

export interface ChangeGroup {
  id: ChangeGroupId;
  /** Section heading. */
  title: string;
  /** Why the group is called out, when that needs saying. */
  note: string | undefined;
  changes: StagedChange[];
}

export const CONFLICT_EXPLANATION =
  "These files changed on disk after Commonspace prepared these changes. Applying one would overwrite whatever changed.";

/**
 * Groups in a fixed order, most consequential first: what a person must not
 * miss belongs at the top of the list rather than below a scroll. Conflicts
 * come out of their kind group entirely — a conflicted deletion and a
 * conflicted edit share the problem that matters more than the kind does.
 */
const GROUPS: readonly { id: ChangeGroupId; title: string; note: string | undefined }[] = [
  { id: "conflicted", title: "Needs your attention", note: CONFLICT_EXPLANATION },
  { id: "delete", title: "Files to delete", note: undefined },
  { id: "relocate", title: "Files to rename or move", note: undefined },
  { id: "modify", title: "Files to change", note: undefined },
  { id: "create", title: "New files", note: undefined },
];

function groupIdOf(change: StagedChange): ChangeGroupId {
  if (change.conflicted) return "conflicted";
  switch (change.kind) {
    case "delete":
      return "delete";
    case "rename":
    case "move":
      return "relocate";
    case "modify":
      return "modify";
    case "create":
      return "create";
  }
}

/**
 * Group a task's staged changes for review. Empty groups are dropped, and
 * within a group changes are ordered by the file they affect so the same set
 * always reads the same way twice.
 */
export function groupChanges(changes: StagedChange[]): ChangeGroup[] {
  const groups: ChangeGroup[] = [];
  for (const template of GROUPS) {
    const members = changes
      .filter((change) => groupIdOf(change) === template.id)
      .sort((a, b) => a.target.localeCompare(b.target) || a.id.localeCompare(b.id));
    if (members.length > 0) groups.push({ ...template, changes: members });
  }
  return groups;
}

/** Every change in review order, flattened — the order Apply reports in. */
export function orderedChanges(changes: StagedChange[]): StagedChange[] {
  return groupChanges(changes).flatMap((group) => group.changes);
}

/* -------------------------------------------------------------- selection */

/**
 * The changes Apply may take without a deliberate opt-in. A conflicted change
 * is never one of them: the file moved under the proposal, so applying it
 * would silently discard work the user did themselves.
 */
export function applicableIds(changes: StagedChange[]): string[] {
  return changes.filter((change) => !change.conflicted).map((change) => change.id);
}

export function selectAll(changes: StagedChange[]): Set<string> {
  return new Set(applicableIds(changes));
}

export function selectNone(): Set<string> {
  return new Set<string>();
}

export function toggleSelection(selected: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(selected);
  if (!next.delete(id)) next.add(id);
  return next;
}

/** True when every applicable change is selected (and there is one to select). */
export function allApplicableSelected(
  changes: StagedChange[],
  selected: ReadonlySet<string>,
): boolean {
  const ids = applicableIds(changes);
  return ids.length > 0 && ids.every((id) => selected.has(id));
}

/**
 * Drop selected ids that are no longer staged. The list reloads after every
 * apply and discard, and a selection remembering ids that are gone would make
 * the Apply button count changes that no longer exist.
 */
export function pruneSelection(
  selected: ReadonlySet<string>,
  changes: StagedChange[],
): Set<string> {
  const live = new Set(changes.map((change) => change.id));
  return new Set([...selected].filter((id) => live.has(id)));
}

export function selectedChanges(
  changes: StagedChange[],
  selected: ReadonlySet<string>,
): StagedChange[] {
  return orderedChanges(changes).filter((change) => selected.has(change.id));
}

/* --------------------------------------------------------------- wording */

function plural(count: number, singular: string, pluralForm: string): string {
  return count === 1 ? singular : pluralForm;
}

/**
 * The one line at the top of the Studio: what this set of proposals would do
 * to the user's folders, and how much of it is not safe to wave through.
 */
export function pendingSummary(changes: StagedChange[]): string {
  if (changes.length === 0) return "Nothing is waiting for your review.";
  const files = new Set(changes.map((change) => change.target)).size;
  const conflicted = new Set(
    changes.filter((change) => change.conflicted).map((change) => change.target),
  ).size;
  const head = `${formatCount(files)} ${plural(files, "file", "files")} will change`;
  if (conflicted === 0) return `${head}.`;
  return `${head}, ${formatCount(conflicted)} ${plural(conflicted, "needs", "need")} your attention.`;
}

/**
 * The line the Files panel shows to point at the Studio. The Files panel
 * lists what is already on disk, so a proposal can only be announced there,
 * never listed among the real files.
 */
export function pendingNotice(count: number): string | undefined {
  if (count <= 0) return undefined;
  return count === 1
    ? "1 change is waiting for your review."
    : `${formatCount(count)} changes are waiting for your review.`;
}

/**
 * The Apply button's own words. The count is in the label rather than beside
 * it, because the label is what a screen reader reads when focus lands on the
 * button and what a hurried person reads before pressing it.
 */
export function applyLabel(count: number): string {
  if (count === 0) return "Apply changes";
  return `Apply ${formatCount(count)} ${plural(count, "change", "changes")}`;
}

export function discardLabel(count: number): string {
  if (count === 0) return "Discard changes";
  return `Discard ${formatCount(count)} ${plural(count, "change", "changes")}`;
}

/**
 * The warning above Apply when the selection contains a change whose file has
 * since moved on. Nothing is blocked — the user may still have good reason —
 * but they are told exactly what pressing the button costs.
 */
export function conflictWarning(count: number): string | undefined {
  if (count <= 0) return undefined;
  if (count === 1) {
    return "1 change you selected was prepared before that file changed on disk. Applying it will overwrite that change.";
  }
  return `${formatCount(count)} changes you selected were prepared before those files changed on disk. Applying them will overwrite those changes.`;
}

/** What Discard is about to do, said before it happens rather than after. */
export function discardConfirmation(count: number): string {
  if (count === 1) {
    return "Discard this change? Commonspace throws it away and your file stays as it is.";
  }
  return `Discard ${formatCount(count)} changes? Commonspace throws them away and your files stay as they are.`;
}

/**
 * One calm line after an apply, honest about partial failure the way
 * `summarizeUndo` is: an operation can legitimately refuse, and reporting
 * "done" over the top of that would be the worst thing this screen could do.
 */
export function summarizeApplied(results: OperationResult[]): string {
  if (results.length === 0) return "Nothing was applied.";
  const applied = results.filter((result) => result.success).length;
  const total = results.length;
  if (applied === total) {
    return `${formatCount(total)} ${plural(total, "change", "changes")} applied to your files.`;
  }
  if (applied === 0) {
    return total === 1
      ? "That change could not be applied. Your file was left as it is."
      : "None of these changes could be applied. Your files were left as they are.";
  }
  return `${formatCount(applied)} of ${formatCount(total)} changes applied. The rest could not be, and those files were left as they are.`;
}

export function summarizeDiscarded(count: number): string {
  if (count <= 0) return "Nothing was discarded.";
  return `${formatCount(count)} ${plural(count, "change", "changes")} discarded. Your files were not touched.`;
}

/* ------------------------------------------------------------ diff wording */

/** How much moved, in lines, for the header above a diff. */
export function describeDiffTotals(
  added: number,
  removed: number,
  locale?: string | undefined,
): string {
  if (added === 0 && removed === 0) return "No lines added or removed.";
  if (added > 0 && removed > 0) {
    return `${formatCount(added, locale)} ${plural(added, "line", "lines")} added, ${formatCount(removed, locale)} removed.`;
  }
  if (added > 0) {
    return `${formatCount(added, locale)} ${plural(added, "line", "lines")} added.`;
  }
  return `${formatCount(removed, locale)} ${plural(removed, "line", "lines")} removed.`;
}

/** Why a change has no line-by-line comparison. */
export function missingDiffReason(
  reason: NonNullable<ChangePreview["summary"]>["reason"],
): string {
  return reason === "too_large"
    ? "This file is too large to compare line by line."
    : "This file is not text, so there is nothing to compare line by line.";
}

/**
 * The shape of a change that could not be diffed: before and after, in sizes
 * and line counts. Without this the panel would be blank, and a blank panel
 * reads as "nothing is going to happen" — the one thing it must never say
 * when something is.
 */
export function diffShapeRows(
  summary: NonNullable<ChangePreview["summary"]>,
  locale?: string | undefined,
): { label: string; value: string }[] {
  const row = (label: string, bytes: number, lines: number | null | undefined) => {
    const size = formatFileSize(bytes, locale);
    const value =
      lines === null || lines === undefined
        ? size
        : `${size}, ${formatCount(lines, locale)} ${plural(lines, "line", "lines")}`;
    return { label, value };
  };
  return [
    row("Before", summary.old_bytes, summary.old_lines),
    row("After", summary.new_bytes, summary.new_lines),
  ];
}

/**
 * What to say when the diff came back with no hunks at all. For an Office
 * file this is the everyday case — the words are identical and only the
 * formatting moved — so the sentence has to be precise about what was
 * compared, and the preview's caveat carries the rest.
 */
export function noDifferenceNote(basis: ChangePreview["basis"]): string {
  return basis === "extracted_text"
    ? "The text Commonspace can read from this file is unchanged."
    : "The text in this file is unchanged.";
}

/**
 * A quiet line explaining what was compared, when the backend did not already
 * say it. The backend's caveat is more specific than anything that could be
 * inferred here, so when there is one it speaks alone rather than being
 * paraphrased underneath.
 */
export function basisNote(
  basis: ChangePreview["basis"],
  caveat: string | null | undefined,
): string | undefined {
  if (caveat) return undefined;
  if (basis === "full_text") return undefined;
  return "Compared using the text Commonspace extracted from this file, not its formatting.";
}

/** Hunk heading: which lines of the file this stretch covers. */
export function hunkRange(
  hunk: { new_start: number; new_lines: number },
  locale?: string | undefined,
): string {
  if (hunk.new_lines <= 0) return `Line ${formatCount(hunk.new_start, locale)}`;
  const end = hunk.new_start + hunk.new_lines - 1;
  if (end === hunk.new_start) return `Line ${formatCount(hunk.new_start, locale)}`;
  return `Lines ${formatCount(hunk.new_start, locale)}–${formatCount(end, locale)}`;
}
