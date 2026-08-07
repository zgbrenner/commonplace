import { useEffect, useRef, useState } from "react";
import type { Artifact, ArtifactKind, OperationResult } from "@commonspace/protocol";
import { formatDateTime } from "../lib/format";
import { pendingNotice } from "../lib/staging";
import { Button, Card, PanelHeading, StatusPill } from "./primitives";

export interface ArtifactPanelProps {
  /** Files that are already real: created or changed on disk. */
  artifacts: Artifact[];
  /**
   * How many proposed changes are still waiting in the Artifact Studio, if
   * any. Proposals are not files yet, so they are never listed here — the
   * panel only points at them.
   */
  pendingCount?: number | undefined;
  /** Opens the Studio. Without it the pending notice is only a statement. */
  onReviewPending?: (() => void) | undefined;
  onOpen: (artifact: Artifact) => void;
  onReveal: (artifact: Artifact) => void;
  onUndo: (artifact: Artifact) => Promise<OperationResult>;
}

/**
 * The right-hand panel: files the task has actually written.
 *
 * Its counterpart is the Artifact Studio, which holds changes that have been
 * proposed and not yet applied. The two stay separate on purpose — everything
 * in this panel exists on disk and can be opened, shown in a folder, or
 * undone, and none of that is true of a proposal. Mixing them would put "this
 * is your file" and "this might become your file" in one list.
 */
export function ArtifactPanel({
  artifacts,
  pendingCount = 0,
  onReviewPending,
  onOpen,
  onReveal,
  onUndo,
}: ArtifactPanelProps) {
  return (
    <aside
      aria-label="Files"
      className="flex w-80 shrink-0 flex-col border-l border-[var(--color-line)] bg-[var(--color-surface-sunken)]"
    >
      {pendingCount > 0 ? (
        <PendingNotice count={pendingCount} onReview={onReviewPending} />
      ) : null}

      <PanelHeading>Files</PanelHeading>
      {artifacts.length === 0 ? (
        <p className="px-4 pb-4 text-sm text-[var(--color-ink-faint)]">
          Files Commonspace creates or changes will appear here, with a way to open or undo
          them.
        </p>
      ) : (
        <ul className="min-h-0 flex-1 space-y-2 overflow-y-auto px-3 pb-4">
          {artifacts.map((artifact) => (
            <ArtifactCard
              key={artifact.id}
              artifact={artifact}
              onOpen={onOpen}
              onReveal={onReveal}
              onUndo={onUndo}
            />
          ))}
        </ul>
      )}
    </aside>
  );
}

/**
 * Proposals waiting in the Studio, announced here because this panel is where
 * a person looks for "what happened to my files" — and the honest answer,
 * while changes are staged, is "nothing yet, and here is where to decide".
 */
function PendingNotice({ count, onReview }: { count: number; onReview: (() => void) | undefined }) {
  const line = pendingNotice(count);
  if (!line) return null;
  return (
    <div className="border-b border-[var(--color-line)] px-4 py-3">
      <p className="text-sm text-[var(--color-ink)]">
        <span aria-hidden="true" className="mr-1.5 text-[var(--color-warn)]">
          ●
        </span>
        {line}
      </p>
      <p className="mt-0.5 text-xs text-[var(--color-ink-faint)]">
        They have not touched your files.
      </p>
      {onReview ? (
        <Button size="sm" variant="primary" className="mt-2" onClick={onReview}>
          Review changes
        </Button>
      ) : null}
    </div>
  );
}

function ArtifactCard({
  artifact,
  onOpen,
  onReveal,
  onUndo,
}: {
  artifact: Artifact;
  onOpen: (artifact: Artifact) => void;
  onReveal: (artifact: Artifact) => void;
  onUndo: (artifact: Artifact) => Promise<OperationResult>;
}) {
  const [undoState, setUndoState] = useState<
    { kind: "idle" } | { kind: "working" } | { kind: "done"; result: OperationResult }
  >({ kind: "idle" });
  const undoResultRef = useRef<HTMLParagraphElement>(null);

  // The Undo button is replaced by its own result, so a keyboard user's
  // focus would otherwise land back at the top of the page. Moving it onto
  // the result both keeps their place and reads them the answer.
  useEffect(() => {
    if (undoState.kind === "done") undoResultRef.current?.focus();
  }, [undoState.kind]);

  const undo = async () => {
    setUndoState({ kind: "working" });
    const result = await onUndo(artifact);
    setUndoState({ kind: "done", result });
  };

  const canUndo = Boolean(artifact.file_operation_id) && undoState.kind !== "done";
  // The reader's date order and clock, not ours: 07/08 reads as two
  // different days on two sides of an ocean, and this is the line someone
  // checks to be sure they are looking at the right version of a file.
  const when = formatDateTime(artifact.created_at);

  return (
    <Card as="li" className="p-3">
      <div className="flex items-start gap-2.5">
        <FileGlyph kind={artifact.kind} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium" title={artifact.path}>
            {artifact.name}
          </p>
          <p className="mt-0.5">
            <StatusPill tone={artifact.modified_existing ? "warn" : "ok"} glyph={artifact.modified_existing ? "±" : "+"}>
              {artifact.modified_existing ? "Changed" : "Created"}
            </StatusPill>
          </p>
          {when ? (
            <p className="mt-1 text-xs text-[var(--color-ink-faint)]">
              <span className="sr-only">
                {artifact.modified_existing ? "Changed on " : "Created on "}
              </span>
              {when}
            </p>
          ) : null}
        </div>
      </div>

      {artifact.change_summary ? (
        <p className="mt-2 text-xs text-[var(--color-ink-muted)]">{artifact.change_summary}</p>
      ) : null}

      {artifact.backup_path ? (
        <p className="mt-1.5 truncate text-xs text-[var(--color-ink-faint)]" title={artifact.backup_path}>
          Backup kept at {artifact.backup_path.split(/[/\\]/).slice(-2).join("/")}
        </p>
      ) : null}

      <div className="mt-2.5 flex flex-wrap gap-1.5">
        <Button size="sm" onClick={() => onOpen(artifact)}>
          Open
        </Button>
        <Button size="sm" variant="quiet" onClick={() => onReveal(artifact)}>
          Show in folder
        </Button>
        {canUndo ? (
          <Button size="sm" variant="quiet" onClick={undo} disabled={undoState.kind === "working"}>
            {undoState.kind === "working" ? "Undoing…" : "Undo"}
          </Button>
        ) : null}
      </div>

      {undoState.kind === "done" ? (
        <p
          ref={undoResultRef}
          tabIndex={-1}
          className={`mt-2 text-xs ${
            undoState.result.success ? "text-[var(--color-ok)]" : "text-[var(--color-danger)]"
          }`}
        >
          <span aria-hidden="true" className="mr-1">
            {undoState.result.success ? "✓" : "⚠"}
          </span>
          {undoState.result.user_summary}
          {!undoState.result.success && undoState.result.validation.outcome === "failed" ? (
            <span className="mt-0.5 block text-[var(--color-ink-muted)]">
              {undoState.result.validation.detail}
            </span>
          ) : null}
        </p>
      ) : null}
    </Card>
  );
}

/** A small type glyph — text, so it scales with the user's font size. */
function FileGlyph({ kind }: { kind: ArtifactKind }) {
  const labels: Record<ArtifactKind, string> = {
    docx: "DOC",
    xlsx: "XLS",
    pptx: "PPT",
    pdf: "PDF",
    markdown: "MD",
    text: "TXT",
    image: "IMG",
    code_diff: "DIFF",
    other: "FILE",
  };
  return (
    <span
      aria-hidden="true"
      className="mt-0.5 shrink-0 rounded border border-[var(--color-line-strong)] px-1 py-0.5 text-[0.625rem] font-semibold text-[var(--color-ink-faint)]"
    >
      {labels[kind]}
    </span>
  );
}
