import { useState } from "react";
import type { Artifact, ArtifactKind, OperationResult } from "@commonspace/protocol";
import { Button, Card, PanelHeading, StatusPill } from "./primitives";

interface ArtifactPanelProps {
  artifacts: Artifact[];
  onOpen: (artifact: Artifact) => void;
  onReveal: (artifact: Artifact) => void;
  onUndo: (artifact: Artifact) => Promise<OperationResult>;
}

/** The right-hand panel: what the task produced or changed. */
export function ArtifactPanel({ artifacts, onOpen, onReveal, onUndo }: ArtifactPanelProps) {
  return (
    <aside
      aria-label="Files"
      className="flex w-80 shrink-0 flex-col border-l border-[var(--color-line)] bg-[var(--color-surface-sunken)]"
    >
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

  const undo = async () => {
    setUndoState({ kind: "working" });
    const result = await onUndo(artifact);
    setUndoState({ kind: "done", result });
  };

  const canUndo = Boolean(artifact.file_operation_id) && undoState.kind !== "done";

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
          role="status"
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
