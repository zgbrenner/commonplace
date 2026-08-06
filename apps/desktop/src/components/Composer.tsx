import { useCallback, useRef, useState, type KeyboardEvent } from "react";
import type { Connection, Workspace } from "@commonspace/protocol";
import { attachmentDisclosure, mergePaths } from "../lib/attachments";
import { recommendedProvider, usableConnections } from "../lib/recommend";
import { useFileDrop } from "../lib/useFileDrop";
import { Button } from "./primitives";

interface ComposerProps {
  workspace: Workspace | undefined;
  connections: Connection[];
  provider: string;
  onProviderChange: (provider: string) => void;
  model: string;
  onModelChange: (model: string) => void;
  attachments: string[];
  onAttachmentsChange: (paths: string[]) => void;
  onAttachFiles: () => void;
  onAttachFolder: () => void;
  onSubmit: (prompt: string) => void;
  disabled: boolean;
  disabledReason?: string | undefined;
}

/** The composer: text, attachments, agent and model choice. */
export function Composer({
  workspace,
  connections,
  provider,
  onProviderChange,
  model,
  onModelChange,
  attachments,
  onAttachmentsChange,
  onAttachFiles,
  onAttachFolder,
  onSubmit,
  disabled,
  disabledReason,
}: ComposerProps) {
  const [text, setText] = useState("");
  const [detailsOpen, setDetailsOpen] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Dropped files arrive through Tauri's native drag-drop event with real
  // absolute paths, and merge into the attachment list exactly like the
  // file picker does.
  const dragging = useFileDrop((paths) => onAttachmentsChange(mergePaths(attachments, paths)));

  const usable = usableConnections(connections);
  // With one agent connected there is no choice to make, so the composer
  // states which one is running rather than presenting a menu of one.
  const onlyAgent = usable.length === 1 ? usable[0] : undefined;
  const recommended = recommendedProvider(connections);
  const activeConnection = connections.find((c) => c.provider === provider);
  const models = activeConnection?.capabilities.models ?? [];

  const submit = useCallback(() => {
    const prompt = text.trim();
    if (!prompt || disabled) return;
    // Paths are no longer embedded in the prompt text — App passes the
    // attachment list to the backend as structured data, and the backend
    // handles disclosure to the provider.
    onSubmit(prompt);
    setText("");
    // Clear attachments only after onSubmit: App captures the attachment
    // state during the send, so clearing first would lose it.
    onAttachmentsChange([]);
  }, [text, disabled, onSubmit, onAttachmentsChange]);

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter sends, Shift+Enter makes a new line — the familiar desktop chat
    // convention.
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  return (
    <div
      className={`border-t border-[var(--color-line)] bg-[var(--color-surface)] px-6 py-4 ${
        dragging ? "bg-[var(--color-accent-soft)]" : ""
      }`}
    >
      <div className="mx-auto max-w-3xl">
        {dragging ? (
          <p className="mb-2 rounded-md border border-dashed border-[var(--color-accent)] px-3 py-2 text-center text-sm text-[var(--color-accent)]">
            Drop files to attach
          </p>
        ) : null}

        {attachments.length > 0 ? (
          // A quiet disclosure, not a modal: says plainly what will leave the
          // machine before anything is sent. We count "attached items" rather
          // than files vs folders — the frontend can't reliably tell them
          // apart from a path string alone (see attachmentDisclosure).
          // Deliberately no token estimates; those are deferred to a future
          // Details-level view.
          <div className="mb-2 rounded-md border border-[var(--color-line)] bg-[var(--color-surface-sunken)] px-3 py-2 text-xs text-[var(--color-ink-muted)]">
            <p>{attachmentDisclosure(attachments.length, activeConnection?.display_name)}</p>
            <button
              type="button"
              aria-expanded={detailsOpen}
              onClick={() => setDetailsOpen((open) => !open)}
              className="mt-1 text-[var(--color-ink-faint)] hover:text-[var(--color-ink)]"
            >
              <span
                aria-hidden="true"
                className={`mr-1 inline-block ${detailsOpen ? "rotate-90" : ""}`}
              >
                ›
              </span>
              Details
            </button>
            {detailsOpen ? (
              // The chips below show basenames; this list shows the full
              // paths, with the same removal affordance.
              <ul className="mt-1.5 space-y-1">
                {attachments.map((path) => (
                  <li key={path} className="flex items-center gap-2">
                    <span className="selectable min-w-0 flex-1 truncate font-mono" title={path}>
                      {path}
                    </span>
                    <button
                      type="button"
                      onClick={() => onAttachmentsChange(attachments.filter((p) => p !== path))}
                      className="shrink-0 text-[var(--color-ink-faint)] hover:text-[var(--color-ink)]"
                      aria-label={`Remove ${path}`}
                    >
                      Remove
                    </button>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}

        {attachments.length > 0 ? (
          <ul className="mb-2 flex flex-wrap gap-1.5">
            {attachments.map((path) => (
              <li
                key={path}
                className="flex items-center gap-1.5 rounded-md bg-[var(--color-surface-sunken)] px-2 py-1 text-xs text-[var(--color-ink-muted)]"
              >
                <span className="max-w-[18rem] truncate" title={path}>
                  {path.split(/[/\\]/).pop()}
                </span>
                <button
                  type="button"
                  onClick={() => onAttachmentsChange(attachments.filter((p) => p !== path))}
                  className="text-[var(--color-ink-faint)] hover:text-[var(--color-ink)]"
                  aria-label={`Remove ${path}`}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        ) : null}

        <div className="rounded-[var(--radius-card)] border border-[var(--color-line-strong)] bg-[var(--color-surface-raised)] focus-within:border-[var(--color-accent)]">
          <label htmlFor="composer" className="sr-only">
            What would you like Commonspace to do?
          </label>
          <textarea
            id="composer"
            ref={textareaRef}
            value={text}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={onKeyDown}
            rows={3}
            placeholder={
              workspace
                ? `Ask Commonspace to work with the files in ${workspace.name}…`
                : "Choose a folder to work in first…"
            }
            className="selectable w-full resize-none bg-transparent px-4 py-3 text-sm text-[var(--color-ink)] outline-none placeholder:text-[var(--color-ink-faint)]"
          />

          <div className="flex flex-wrap items-center gap-2 border-t border-[var(--color-line)] px-3 py-2">
            <Button size="sm" variant="quiet" onClick={onAttachFiles}>
              Attach files
            </Button>
            <Button size="sm" variant="quiet" onClick={onAttachFolder}>
              Add folder
            </Button>

            <div className="ml-auto flex items-center gap-2">
              {onlyAgent ? (
                <p className="text-xs text-[var(--color-ink-muted)]">
                  Using {onlyAgent.display_name}
                </p>
              ) : (
                <>
                  <label htmlFor="agent" className="sr-only">
                    Agent
                  </label>
                  <select
                    id="agent"
                    value={provider}
                    onChange={(event) => onProviderChange(event.target.value)}
                    className="rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] px-2 py-1 text-xs text-[var(--color-ink-muted)]"
                  >
                    {usable.length === 0 ? (
                      <option value="">No agent connected</option>
                    ) : (
                      usable.map((connection) => (
                        <option key={connection.provider} value={connection.provider}>
                          {connection.provider === recommended
                            ? // Named in the option text rather than shown as a
                              // separate badge: a select can only carry text, and
                              // the label has to survive the closed state too.
                              `${connection.display_name} (recommended)`
                            : connection.display_name}
                        </option>
                      ))
                    )}
                  </select>
                </>
              )}

              {models.length > 1 ? (
                <>
                  <label htmlFor="model" className="sr-only">
                    Model
                  </label>
                  <select
                    id="model"
                    value={model}
                    onChange={(event) => onModelChange(event.target.value)}
                    className="rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] px-2 py-1 text-xs text-[var(--color-ink-muted)]"
                  >
                    {models.map((name) => (
                      <option key={name} value={name}>
                        {name === "default" ? "Default model" : name}
                      </option>
                    ))}
                  </select>
                </>
              ) : null}

              <Button
                variant="primary"
                size="sm"
                onClick={submit}
                disabled={disabled || text.trim().length === 0}
              >
                Send
              </Button>
            </div>
          </div>
        </div>

        {disabled && disabledReason ? (
          <p className="mt-2 text-xs text-[var(--color-ink-muted)]">{disabledReason}</p>
        ) : (
          <p className="mt-2 text-xs text-[var(--color-ink-faint)]">
            Commonspace asks before changing, moving, or deleting anything.
          </p>
        )}
      </div>
    </div>
  );
}
