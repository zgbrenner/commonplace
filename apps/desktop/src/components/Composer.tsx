import { useCallback, useRef, useState, type DragEvent, type KeyboardEvent } from "react";
import type { Connection, Workspace } from "@commonspace/protocol";
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
  const [dragging, setDragging] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const usable = connections.filter(
    (c) => c.auth.status === "subscription" || c.auth.status === "api_key" || c.auth.status === "local_model",
  );
  const activeConnection = connections.find((c) => c.provider === provider);
  const models = activeConnection?.capabilities.models ?? [];

  const submit = useCallback(() => {
    const prompt = text.trim();
    if (!prompt || disabled) return;
    const withAttachments =
      attachments.length > 0
        ? `${prompt}\n\nFiles and folders I've attached:\n${attachments.map((p) => `- ${p}`).join("\n")}`
        : prompt;
    onSubmit(withAttachments);
    setText("");
    onAttachmentsChange([]);
  }, [text, disabled, attachments, onSubmit, onAttachmentsChange]);

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter sends, Shift+Enter makes a new line — the familiar desktop chat
    // convention.
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  const onDrop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    setDragging(false);
    // The webview only exposes names for dropped files; Tauri's own drag-drop
    // event carries real paths and is wired in App. This handler keeps the
    // affordance visible and guides the user to the picker.
    if (event.dataTransfer.files.length > 0) {
      onAttachFiles();
    }
  };

  return (
    <div
      className={`border-t border-[var(--color-line)] bg-[var(--color-surface)] px-6 py-4 ${
        dragging ? "bg-[var(--color-accent-soft)]" : ""
      }`}
      onDragOver={(event) => {
        event.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={onDrop}
    >
      <div className="mx-auto max-w-3xl">
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
                      {connection.display_name}
                    </option>
                  ))
                )}
              </select>

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
