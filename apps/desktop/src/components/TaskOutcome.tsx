import { useState } from "react";
import type { Artifact, OperationResult } from "@commonspace/protocol";
import { summarizeUndo, type TaskOutcome as TaskOutcomeModel } from "../lib/replay";
import { Button, Card, ErrorNotice, StatusPill } from "./primitives";

interface TaskOutcomeProps {
  outcome: TaskOutcomeModel;
  onOpen: (artifact: Artifact) => void;
  onReveal: (artifact: Artifact) => void;
  onUndoArtifact: (artifact: Artifact) => Promise<OperationResult>;
  onUndoTask: () => Promise<OperationResult[]>;
}

/**
 * The outcome card at the end of a task's timeline: what happened, which
 * files changed, and the way back (undo). The same card renders for a task
 * that just finished on screen and for one replayed from history, so the
 * two views converge. Render it with `key={outcome.taskId}` so undo state
 * resets when the task changes.
 */
export function TaskOutcome({
  outcome,
  onOpen,
  onReveal,
  onUndoArtifact,
  onUndoTask,
}: TaskOutcomeProps) {
  const [undoState, setUndoState] = useState<
    | { kind: "idle" }
    | { kind: "working" }
    | { kind: "done"; message: string; allUndone: boolean }
  >({ kind: "idle" });

  if (outcome.kind === "none") return null;

  const undoTask = async () => {
    setUndoState({ kind: "working" });
    const results = await onUndoTask();
    setUndoState({
      kind: "done",
      message: summarizeUndo(results),
      allUndone: results.length > 0 && results.every((result) => result.success),
    });
  };

  return (
    <Card as="section" className="p-4" aria-labelledby="task-outcome-heading">
      <div className="flex items-start justify-between gap-3">
        <h3 id="task-outcome-heading" className="text-sm font-semibold">
          {outcome.headline}
        </h3>
        <OutcomePill kind={outcome.kind} />
      </div>

      {outcome.summary ? (
        <p className="selectable mt-1.5 text-sm whitespace-pre-wrap text-[var(--color-ink)]">
          {outcome.summary}
        </p>
      ) : null}

      {outcome.error ? (
        <div className="mt-2.5">
          <ErrorNotice message={outcome.error.message} recovery={outcome.error.recovery} />
        </div>
      ) : null}

      {outcome.note ? (
        <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">{outcome.note}</p>
      ) : null}

      {outcome.deniedNote ? (
        <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">
          <span aria-hidden="true" className="mr-1.5 text-[var(--color-warn)]">
            ⊘
          </span>
          {outcome.deniedNote}
        </p>
      ) : null}

      {outcome.artifacts.length > 0 ? (
        <div className="mt-3">
          <h4 className="text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
            Files created or changed
          </h4>
          <ul className="mt-1.5 space-y-1.5">
            {outcome.artifacts.map((artifact) => (
              <OutcomeFileRow
                key={artifact.id}
                artifact={artifact}
                onOpen={onOpen}
                onReveal={onReveal}
                onUndo={onUndoArtifact}
              />
            ))}
          </ul>
        </div>
      ) : null}

      {outcome.canUndoTask && undoState.kind !== "done" ? (
        <div className="mt-3">
          <Button size="sm" onClick={() => void undoTask()} disabled={undoState.kind === "working"}>
            {undoState.kind === "working" ? "Undoing…" : "Undo this task"}
          </Button>
        </div>
      ) : null}

      {undoState.kind === "done" ? (
        <p
          role="status"
          className={`mt-2 text-sm ${
            undoState.allUndone ? "text-[var(--color-ok)]" : "text-[var(--color-warn)]"
          }`}
        >
          <span aria-hidden="true" className="mr-1">
            {undoState.allUndone ? "✓" : "⚠"}
          </span>
          {undoState.message}
        </p>
      ) : null}
    </Card>
  );
}

function OutcomePill({ kind }: { kind: TaskOutcomeModel["kind"] }) {
  switch (kind) {
    case "completed":
      return (
        <StatusPill tone="ok" glyph="✓">
          Done
        </StatusPill>
      );
    case "failed":
      return (
        <StatusPill tone="danger" glyph="✕">
          Didn&apos;t finish
        </StatusPill>
      );
    case "cancelled":
      return (
        <StatusPill tone="neutral" glyph="■">
          Stopped
        </StatusPill>
      );
    case "interrupted":
      return (
        <StatusPill tone="warn" glyph="!">
          Interrupted
        </StatusPill>
      );
    default:
      return null;
  }
}

/** A compact file row with the same affordances as the Files panel. */
function OutcomeFileRow({
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
    <li className="rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-sm font-medium" title={artifact.path}>
          {artifact.name}
        </span>
        <StatusPill
          tone={artifact.modified_existing ? "warn" : "ok"}
          glyph={artifact.modified_existing ? "±" : "+"}
        >
          {artifact.modified_existing ? "Changed" : "Created"}
        </StatusPill>
        <span className="flex shrink-0 gap-1">
          <Button size="sm" variant="quiet" onClick={() => onOpen(artifact)}>
            Open
          </Button>
          <Button size="sm" variant="quiet" onClick={() => onReveal(artifact)}>
            Show in folder
          </Button>
          {canUndo ? (
            <Button
              size="sm"
              variant="quiet"
              onClick={() => void undo()}
              disabled={undoState.kind === "working"}
            >
              {undoState.kind === "working" ? "Undoing…" : "Undo"}
            </Button>
          ) : null}
        </span>
      </div>
      {undoState.kind === "done" ? (
        <p
          role="status"
          className={`mt-1.5 text-xs ${
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
    </li>
  );
}
