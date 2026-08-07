import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { OperationResult } from "@commonspace/protocol";
import { CommonspaceError, stagedDiff, type ChangePreview, type StagedChange } from "../lib/ipc";
import { formatDateTime } from "../lib/format";
import {
  allApplicableSelected,
  applicableIds,
  applyLabel,
  conflictWarning,
  destinationLine,
  discardConfirmation,
  discardLabel,
  fileNameOf,
  groupChanges,
  kindGlyph,
  kindLabel,
  kindTone,
  orderedChanges,
  pendingSummary,
  pruneSelection,
  selectAll,
  selectNone,
  selectedChanges,
  sizeLine,
  summarizeApplied,
  summarizeDiscarded,
  toggleSelection,
} from "../lib/staging";
import { DiffView } from "./DiffView";
import { Button, Card, EmptyState, ErrorNotice, StatusPill } from "./primitives";

export interface ArtifactStudioProps {
  /** The task whose proposals these are; also what previews are fetched for. */
  taskId: string;
  /** Everything staged for that task, exactly as `listStagedChanges` returned it. */
  changes: StagedChange[];
  /** True while the caller is loading or reloading `changes`. */
  loading?: boolean | undefined;
  /** Set when the list itself could not be loaded; shown in place of the list. */
  loadError?: { message: string; recovery?: string | undefined } | undefined;
  /** Reload the list. Offered as the recovery action when loading failed. */
  onReload?: (() => void) | undefined;
  /**
   * Apply these changes to the user's files, resolving with one result per
   * change. The caller is expected to reload `changes` afterwards: whatever
   * applied has left the staging area, and whatever refused has not.
   */
  onApply: (changeIds: string[]) => Promise<OperationResult[]>;
  /** Throw these changes away. The user's files are not touched. */
  onDiscard: (changeIds: string[]) => Promise<void>;
  /**
   * Leave the review and go back to the conversation. A surface this size
   * with no way out is a trap, so when the caller mounts it in place of the
   * conversation it should pass this.
   */
  onClose?: (() => void) | undefined;
  /**
   * Fetch one change's comparison. Defaults to `stagedDiff` over IPC; the
   * parameter exists so the surface can be driven from somewhere else.
   */
  loadPreview?: ((changeId: string) => Promise<ChangePreview>) | undefined;
}

type PreviewState =
  | { status: "loading"; changeId: string }
  | { status: "ready"; changeId: string; preview: ChangePreview }
  | { status: "error"; changeId: string; error: { message: string; recovery: string | undefined } };

/**
 * The Artifact Studio: proposed output, before it becomes a real file.
 *
 * Everything listed here exists only in Commonspace's staging area. The
 * question this screen has to answer, for someone who is not technical and
 * has no reason to trust us, is "what is this about to do to my documents?" —
 * so the list says it in words, the pane beside it shows it line by line, and
 * the button that makes it real always names how many files it will touch.
 *
 * The rules behind the grouping, the selection and every sentence here live
 * in `lib/staging.ts`, where they are unit tested.
 */
export function ArtifactStudio({
  taskId,
  changes,
  loading = false,
  loadError,
  onReload,
  onApply,
  onDiscard,
  onClose,
  loadPreview,
}: ArtifactStudioProps) {
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => selectAll(changes));
  const [activeId, setActiveId] = useState<string | undefined>(
    () => orderedChanges(changes)[0]?.id,
  );
  const [preview, setPreview] = useState<PreviewState | undefined>();
  const [busy, setBusy] = useState<"idle" | "applying" | "discarding">("idle");
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const [result, setResult] = useState<{ message: string; complete: boolean } | undefined>();
  const [actionError, setActionError] = useState<
    { message: string; recovery: string | undefined } | undefined
  >();

  // Ids are scoped to this instance: the Studio is a surface, and two of
  // them on one screen must not share a checkbox label.
  const domId = useId();
  const selectAllId = `${domId}-select-all`;
  const noteId = (group: string) => `${domId}-note-${group}`;

  const resultRef = useRef<HTMLParagraphElement>(null);
  const confirmRef = useRef<HTMLParagraphElement>(null);
  const selectAllRef = useRef<HTMLInputElement>(null);
  /** Ids already offered to the user, so a reload cannot silently re-tick a box they cleared. */
  const knownIds = useRef<ReadonlySet<string>>(new Set(changes.map((change) => change.id)));

  const groups = useMemo(() => groupChanges(changes), [changes]);
  const chosen = useMemo(() => selectedChanges(changes, selected), [changes, selected]);
  const applicable = useMemo(() => applicableIds(changes), [changes]);
  const allSelected = allApplicableSelected(changes, selected);
  const conflictedChosen = chosen.filter((change) => change.conflicted).length;
  const warning = conflictWarning(conflictedChosen);
  const active = changes.find((change) => change.id === activeId);

  // The list is reloaded after every apply and discard. A selection has to
  // survive that: ids that are gone drop out, ids the user has never seen
  // arrive selected unless they are conflicted, and anything they deliberately
  // ticked or cleared stays as they left it.
  useEffect(() => {
    setSelected((current) => {
      const next = pruneSelection(current, changes);
      for (const change of changes) {
        if (!knownIds.current.has(change.id) && !change.conflicted) next.add(change.id);
      }
      knownIds.current = new Set(changes.map((change) => change.id));
      return sameMembers(current, next) ? current : next;
    });
    setActiveId((current) => {
      if (current && changes.some((change) => change.id === current)) return current;
      return orderedChanges(changes)[0]?.id;
    });
  }, [changes]);

  // A partly-filled selection is neither checked nor unchecked, and only the
  // DOM property can say so.
  useEffect(() => {
    if (selectAllRef.current) {
      selectAllRef.current.indeterminate = !allSelected && applicable.some((id) => selected.has(id));
    }
  }, [allSelected, applicable, selected]);

  const load = useCallback(
    (changeId: string) => (loadPreview ? loadPreview(changeId) : stagedDiff(taskId, changeId)),
    [loadPreview, taskId],
  );

  useEffect(() => {
    if (!activeId) {
      setPreview(undefined);
      return;
    }
    let cancelled = false;
    setPreview({ status: "loading", changeId: activeId });
    load(activeId)
      .then((fetched) => {
        if (!cancelled) setPreview({ status: "ready", changeId: activeId, preview: fetched });
      })
      .catch((error: unknown) => {
        if (!cancelled) setPreview({ status: "error", changeId: activeId, error: describe(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [activeId, load]);

  // The buttons are gone by the time their answer appears, and a keyboard
  // user's place with them. Focus follows the answer, the way it does after an
  // undo elsewhere in the app.
  useEffect(() => {
    if (result) resultRef.current?.focus();
  }, [result]);

  // The question a discard asks takes focus, so it is read rather than
  // stumbled into — and focus lands on the question, not on the button that
  // answers it destructively.
  useEffect(() => {
    if (confirmingDiscard) confirmRef.current?.focus();
  }, [confirmingDiscard]);

  const apply = async () => {
    const ids = chosen.map((change) => change.id);
    if (ids.length === 0) return;
    setBusy("applying");
    setActionError(undefined);
    setResult(undefined);
    try {
      const results = await onApply(ids);
      setResult({
        message: summarizeApplied(results),
        complete: results.length > 0 && results.every((one) => one.success),
      });
    } catch (error: unknown) {
      setActionError(describe(error));
    } finally {
      setBusy("idle");
    }
  };

  const discard = async () => {
    const ids = chosen.map((change) => change.id);
    if (ids.length === 0) return;
    setBusy("discarding");
    setActionError(undefined);
    setResult(undefined);
    try {
      await onDiscard(ids);
      setResult({ message: summarizeDiscarded(ids.length), complete: true });
      setConfirmingDiscard(false);
    } catch (error: unknown) {
      setActionError(describe(error));
    } finally {
      setBusy("idle");
    }
  };

  const working = busy !== "idle";

  return (
    <section
      aria-labelledby={`${domId}-heading`}
      className="studio flex min-h-0 flex-1 flex-col bg-[var(--color-surface)]"
    >
      <header className="border-b border-[var(--color-line)] px-5 py-4">
        <div className="flex items-start justify-between gap-4">
          <h2 id={`${domId}-heading`} className="text-base font-semibold">
            Changes waiting for your review
          </h2>
          {onClose ? (
            <Button size="sm" variant="quiet" className="shrink-0" onClick={onClose}>
              Back to the conversation
            </Button>
          ) : null}
        </div>
        <p className="mt-1 text-sm text-[var(--color-ink-muted)]">{pendingSummary(changes)}</p>
        <p className="mt-0.5 text-xs text-[var(--color-ink-faint)]">
          None of this has touched your files. Nothing is written until you apply it.
        </p>

        {changes.length > 0 ? (
          <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-2">
            {applicable.length > 0 ? (
              <span className="flex items-center gap-2">
                <input
                  ref={selectAllRef}
                  id={selectAllId}
                  type="checkbox"
                  checked={allSelected}
                  disabled={working}
                  onChange={() => setSelected(allSelected ? selectNone() : selectAll(changes))}
                  className="h-4 w-4 accent-[var(--color-accent)]"
                />
                <label htmlFor={selectAllId} className="text-sm text-[var(--color-ink-muted)]">
                  Select all {applicable.length} that can be applied
                </label>
              </span>
            ) : null}

            <Button
              variant="primary"
              onClick={() => void apply()}
              disabled={chosen.length === 0 || working}
            >
              {busy === "applying" ? "Applying…" : applyLabel(chosen.length)}
            </Button>

            {/* Discard sits at the far end of the row and asks once more
                before it acts: it is the button a mis-click would hurt most. */}
            {confirmingDiscard ? null : (
              <Button
                variant="quiet"
                className="ml-auto"
                onClick={() => setConfirmingDiscard(true)}
                disabled={chosen.length === 0 || working}
              >
                {discardLabel(chosen.length)}
              </Button>
            )}
          </div>
        ) : null}

        {warning ? (
          <p className="mt-2.5 text-sm text-[var(--color-warn)]">
            <span aria-hidden="true" className="mr-1.5">
              ⚠
            </span>
            <span className="sr-only">Warning: </span>
            {warning}
          </p>
        ) : null}

        {confirmingDiscard ? (
          <Card className="mt-2.5 border-[var(--color-line-strong)] p-3">
            <p ref={confirmRef} tabIndex={-1} className="text-sm text-[var(--color-ink)]">
              {discardConfirmation(chosen.length)}
            </p>
            <div className="mt-2 flex flex-wrap gap-2">
              <Button onClick={() => setConfirmingDiscard(false)} disabled={working}>
                Keep them
              </Button>
              <Button variant="danger" onClick={() => void discard()} disabled={working}>
                {busy === "discarding" ? "Discarding…" : discardLabel(chosen.length)}
              </Button>
            </div>
          </Card>
        ) : null}

        {actionError ? (
          <div className="mt-2.5">
            <ErrorNotice message={actionError.message} recovery={actionError.recovery} />
          </div>
        ) : null}

        {result ? (
          <p
            ref={resultRef}
            tabIndex={-1}
            className={`mt-2.5 text-sm ${
              result.complete ? "text-[var(--color-ok)]" : "text-[var(--color-warn)]"
            }`}
          >
            <span aria-hidden="true" className="mr-1">
              {result.complete ? "✓" : "⚠"}
            </span>
            {result.message}
          </p>
        ) : null}
      </header>

      {loadError ? (
        <div className="p-5">
          <ErrorNotice
            message={loadError.message}
            recovery={loadError.recovery}
            onRetry={onReload}
          />
        </div>
      ) : changes.length === 0 ? (
        <div className="min-h-0 flex-1">
          {loading ? (
            <p className="px-5 py-4 text-sm text-[var(--color-ink-muted)]">
              Looking for changes waiting to be reviewed…
            </p>
          ) : (
            <EmptyState
              title="Nothing is waiting for review"
              description="When Commonspace proposes a change to one of your files, it waits here first, with a comparison of what would change. Your files stay as they are until you apply it."
            />
          )}
        </div>
      ) : (
        <div className="studio-layout min-h-0 flex-1">
          <div className="studio-list min-h-0 overflow-y-auto px-4 py-4">
            <ul className="space-y-4">
              {groups.map((group) => (
                <li key={group.id}>
                  <h3 className="text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
                    {group.title}
                  </h3>
                  {group.note ? (
                    <p
                      id={noteId(group.id)}
                      className="mt-1 text-xs text-[var(--color-ink-muted)]"
                    >
                      {group.note}
                    </p>
                  ) : null}
                  <ul className="mt-2 space-y-2">
                    {group.changes.map((change) => (
                      <ChangeRow
                        key={change.id}
                        change={change}
                        active={change.id === activeId}
                        selected={selected.has(change.id)}
                        disabled={working}
                        describedBy={change.conflicted ? noteId(group.id) : undefined}
                        onToggle={() => setSelected(toggleSelection(selected, change.id))}
                        onShow={() => setActiveId(change.id)}
                      />
                    ))}
                  </ul>
                </li>
              ))}
            </ul>
          </div>

          <div className="min-h-0 overflow-y-auto px-5 py-4">
            {active ? (
              <ChangeDetail change={active} preview={preview} />
            ) : (
              <p className="text-sm text-[var(--color-ink-muted)]">
                Choose a change from the list to see what it would do.
              </p>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

/** One proposal: whether it is included, what it is, and the file it affects. */
function ChangeRow({
  change,
  active,
  selected,
  disabled,
  describedBy,
  onToggle,
  onShow,
}: {
  change: StagedChange;
  active: boolean;
  selected: boolean;
  disabled: boolean;
  describedBy: string | undefined;
  onToggle: () => void;
  onShow: () => void;
}) {
  const destination = destinationLine(change);
  const size = sizeLine(change);

  return (
    <li
      // `studio-row` and `studio-row-conflicted` carry no styling of their own;
      // they are the hooks styles.css needs to keep the edge of a row — and of
      // a conflicted one — visible under Windows High Contrast.
      className={`studio-row flex gap-2.5 rounded-[var(--radius-card)] border p-2.5 ${
        change.conflicted
          ? "studio-row-conflicted border-[var(--color-danger)] bg-[var(--color-danger-soft)]"
          : "border-[var(--color-line)] bg-[var(--color-surface-raised)]"
      }`}
    >
      <input
        type="checkbox"
        checked={selected}
        disabled={disabled}
        onChange={onToggle}
        aria-label={`Include in Apply: ${change.summary}`}
        aria-describedby={describedBy}
        className="mt-1 h-4 w-4 shrink-0 accent-[var(--color-accent)]"
      />
      <button
        type="button"
        onClick={onShow}
        aria-current={active ? "true" : undefined}
        className={`min-w-0 flex-1 rounded-md px-1.5 py-1 text-left ${
          active ? "bg-[var(--color-accent-soft)]" : "hover:bg-[var(--color-surface-sunken)]"
        }`}
      >
        <span className="block text-sm font-medium text-[var(--color-ink)]">{change.summary}</span>
        <span className="mt-1 flex flex-wrap items-center gap-1.5">
          <StatusPill tone={kindTone(change.kind)} glyph={kindGlyph(change.kind)}>
            {kindLabel(change.kind)}
          </StatusPill>
          {change.conflicted ? (
            <StatusPill tone="danger" glyph="!">
              Needs your attention
            </StatusPill>
          ) : null}
        </span>
        <span
          className="mt-1 block truncate text-xs text-[var(--color-ink-faint)]"
          title={change.target}
        >
          {change.target}
        </span>
        {destination ? (
          <span className="block truncate text-xs text-[var(--color-ink-muted)]" title={destination}>
            {destination}
          </span>
        ) : null}
        {size ? <span className="block text-xs text-[var(--color-ink-faint)]">{size}</span> : null}
      </button>
    </li>
  );
}

/** The pane beside the list: one change, in full. */
function ChangeDetail({
  change,
  preview,
}: {
  change: StagedChange;
  preview: PreviewState | undefined;
}) {
  const staged = formatDateTime(change.staged_at);
  const showing = preview && preview.changeId === change.id ? preview : undefined;

  return (
    <div>
      <h3 className="text-sm font-semibold break-all">{fileNameOf(change.target)}</h3>
      <p className="mt-0.5 text-xs break-all text-[var(--color-ink-faint)]">{change.target}</p>
      <p className="mt-1.5 flex flex-wrap items-center gap-1.5">
        <StatusPill tone={kindTone(change.kind)} glyph={kindGlyph(change.kind)}>
          {kindLabel(change.kind)}
        </StatusPill>
        {staged ? (
          <span className="text-xs text-[var(--color-ink-faint)]">Prepared {staged}</span>
        ) : null}
      </p>

      {change.conflicted ? (
        <p className="mt-2.5 rounded-md border border-[var(--color-danger)] bg-[var(--color-danger-soft)] px-3 py-2 text-sm text-[var(--color-ink)]">
          <span aria-hidden="true" className="mr-1.5 text-[var(--color-danger)]">
            ⚠
          </span>
          <span className="sr-only">Needs your attention: </span>
          This file changed on disk after Commonspace prepared this change, so the comparison
          below may no longer match what is in the file. Applying it would overwrite whatever
          changed.
        </p>
      ) : null}

      <div className="mt-3">
        {showing === undefined || showing.status === "loading" ? (
          <p className="text-sm text-[var(--color-ink-muted)]">Preparing the comparison…</p>
        ) : showing.status === "error" ? (
          <ErrorNotice
            message={showing.error.message}
            recovery={
              showing.error.recovery ??
              "The change is still staged and untouched. Nothing has been applied."
            }
            announce={false}
          />
        ) : (
          <DiffView preview={showing.preview} fileName={fileNameOf(change.target)} />
        )}
      </div>
    </div>
  );
}

/** Two sets with the same members, compared so an unchanged selection keeps its identity. */
function sameMembers(a: ReadonlySet<string>, b: ReadonlySet<string>): boolean {
  return a.size === b.size && [...a].every((id) => b.has(id));
}

/**
 * An unknown rejection, put into a sentence. The backend for this surface is
 * still being written, so a failure here is expected rather than exceptional —
 * and it has to read as "this did not happen", never as silence.
 */
function describe(error: unknown): { message: string; recovery: string | undefined } {
  if (error instanceof CommonspaceError) {
    return { message: error.message, recovery: error.recovery };
  }
  if (error instanceof Error) return { message: error.message, recovery: undefined };
  return {
    message: "Commonspace could not reach the staging area.",
    recovery: "Your files were not changed. Try again in a moment.",
  };
}
