import type { ChangePreview } from "../lib/ipc";
import {
  basisNote,
  describeDiffTotals,
  diffShapeRows,
  hunkRange,
  missingDiffReason,
  noDifferenceNote,
} from "../lib/staging";

export interface DiffViewProps {
  /** The comparison the backend produced for one staged change. */
  preview: ChangePreview;
  /** The file being compared, used to name the comparison for a screen reader. */
  fileName: string;
}

/**
 * What a proposed change would do to a file, shown line by line.
 *
 * Three things this view will not do. It will not render an empty diff and
 * leave the reader to conclude nothing is happening — a change that could not
 * be compared says why and shows its shape instead. It will not drop the
 * backend's caveat, which for an Office file whose extracted text is
 * identical is the only thing separating "nothing happened" from "the
 * formatting changed". And it will not carry a change on colour alone: every
 * added or removed line has a marker glyph, a left edge, and a word for a
 * screen reader, so the diff survives High Contrast and colour blindness.
 */
export function DiffView({ preview, fileName }: DiffViewProps) {
  const caveat = preview.caveat ?? undefined;
  const basis = basisNote(preview.basis, preview.caveat);
  const summary = preview.summary ?? undefined;
  const hasHunks = preview.hunks.some((hunk) => hunk.lines.length > 0);

  return (
    <div className="text-sm">
      {summary ? (
        <p className="text-[var(--color-ink)]">{missingDiffReason(summary.reason)}</p>
      ) : (
        <p className="text-[var(--color-ink-muted)]">
          {describeDiffTotals(preview.added_lines, preview.removed_lines)}
        </p>
      )}

      {caveat ? <DiffNote glyph="ⓘ" text={caveat} /> : null}

      {basis ? <p className="mt-1.5 text-xs text-[var(--color-ink-faint)]">{basis}</p> : null}

      {preview.truncated ? (
        <DiffNote
          glyph="⋯"
          text="This comparison is too long to show in full. Only the first part of it is below."
        />
      ) : null}

      {summary ? (
        <DiffShape summary={summary} />
      ) : hasHunks ? (
        <DiffTable preview={preview} fileName={fileName} />
      ) : (
        <p className="mt-3 text-[var(--color-ink-muted)]">{noDifferenceNote(preview.basis)}</p>
      )}
    </div>
  );
}

/**
 * A note that must not be skimmed past. `diff-note` carries no styling of its
 * own — it is the hook styles.css needs to keep the left edge visible under
 * Windows High Contrast, where our border colour is replaced.
 */
function DiffNote({ glyph, text }: { glyph: string; text: string }) {
  return (
    <p className="diff-note mt-2 rounded-md border-l-[3px] border-[var(--color-warn)] bg-[var(--color-surface-sunken)] px-3 py-2 text-xs text-[var(--color-ink-muted)]">
      <span aria-hidden="true" className="mr-1.5 text-[var(--color-warn)]">
        {glyph}
      </span>
      <span className="sr-only">Worth knowing: </span>
      {text}
    </p>
  );
}

/** Before and after, for a change that has no line-by-line comparison. */
function DiffShape({ summary }: { summary: NonNullable<ChangePreview["summary"]> }) {
  return (
    <dl className="mt-3 space-y-1 rounded-md bg-[var(--color-surface-sunken)] px-3 py-2.5 text-xs">
      {diffShapeRows(summary).map((row) => (
        <div key={row.label} className="flex flex-wrap gap-x-2">
          <dt className="text-[var(--color-ink-faint)]">{row.label}</dt>
          <dd className="text-[var(--color-ink-muted)]">{row.value}</dd>
        </div>
      ))}
    </dl>
  );
}

function DiffTable({ preview, fileName }: { preview: ChangePreview; fileName: string }) {
  return (
    <div className="mt-3 overflow-x-auto rounded-md border border-[var(--color-line)]">
      <table className="diff-table selectable">
        <caption className="sr-only">Line by line comparison of {fileName}</caption>
        <thead className="sr-only">
          <tr>
            <th scope="col">Line number before</th>
            <th scope="col">Line number after</th>
            <th scope="col">Text</th>
          </tr>
        </thead>
        {preview.hunks.map((hunk, hunkIndex) => (
          <tbody key={`${hunk.old_start}-${hunk.new_start}-${hunkIndex}`}>
            <tr>
              <th scope="rowgroup" colSpan={3} className="diff-hunk-heading">
                {hunkRange(hunk)}
              </th>
            </tr>
            {hunk.lines.map((line, lineIndex) => (
              // Position is the only identity a diff line has: the same text
              // legitimately appears many times in one hunk.
              <DiffLine key={`${hunkIndex}-${lineIndex}`} line={line} />
            ))}
          </tbody>
        ))}
      </table>
    </div>
  );
}

const LINE_MARKERS = { added: "+", removed: "−", context: " " } as const;
const LINE_LABELS = { added: "Added: ", removed: "Removed: ", context: "" } as const;

function DiffLine({
  line,
}: {
  line: ChangePreview["hunks"][number]["lines"][number];
}) {
  const empty = line.spans.every((span) => span.text.length === 0);
  return (
    <tr className={`diff-line diff-line-${line.kind}`}>
      <td className="diff-num">{line.old_line ?? ""}</td>
      <td className="diff-num">{line.new_line ?? ""}</td>
      <td className="diff-text">
        <span aria-hidden="true" className="diff-marker">
          {LINE_MARKERS[line.kind]}
        </span>
        {LINE_LABELS[line.kind] ? (
          <span className="sr-only">{LINE_LABELS[line.kind]}</span>
        ) : null}
        {empty ? (
          // A blank line still has to occupy a row, or an added paragraph
          // break reads as nothing at all.
          <span aria-hidden="true">{" "}</span>
        ) : (
          line.spans.map((span, index) =>
            span.emphasized ? (
              // <mark> rather than a styled span: the emphasis is the reason
              // this line is in the diff at all, and marked text is something
              // a screen reader can announce and High Contrast can colour.
              <mark key={index} className="diff-word">
                {span.text}
              </mark>
            ) : (
              <span key={index}>{span.text}</span>
            ),
          )
        )}
      </td>
    </tr>
  );
}
