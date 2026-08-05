import { useState } from "react";
import type { Workspace } from "@commonspace/protocol";
import { Button, Card, ErrorNotice } from "./primitives";

interface WorkspacesProps {
  workspaces: Workspace[];
  activeId: string | undefined;
  onSelect: (id: string) => void;
  onCreate: (name: string, roots: string[]) => Promise<void>;
  onAddFolder: (workspaceId: string) => Promise<void>;
  onPickFolder: () => Promise<string | undefined>;
}

/**
 * Workspaces: the folders the agent is allowed to touch. Everything outside
 * them requires a fresh, explicit grant through the native picker.
 */
export function Workspaces({
  workspaces,
  activeId,
  onSelect,
  onCreate,
  onAddFolder,
  onPickFolder,
}: WorkspacesProps) {
  const [name, setName] = useState("");
  const [roots, setRoots] = useState<string[]>([]);
  const [error, setError] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);

  const pick = async () => {
    const folder = await onPickFolder();
    if (folder && !roots.includes(folder)) {
      setRoots([...roots, folder]);
      if (!name) {
        setName(folder.split(/[/\\]/).filter(Boolean).pop() ?? "Workspace");
      }
    }
  };

  const create = async () => {
    setBusy(true);
    setError(undefined);
    try {
      await onCreate(name.trim() || "Workspace", roots);
      setName("");
      setRoots([]);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-3xl px-6 py-6">
        <h1 className="text-lg font-semibold">Workspaces</h1>
        <p className="mt-1 text-sm text-[var(--color-ink-muted)]">
          A workspace is the set of folders you&apos;ve authorized. Commonspace can read and
          write inside them; anything else needs your explicit permission first.
        </p>

        <ul className="mt-5 space-y-3">
          {workspaces.map((workspace) => (
            <Card as="li" key={workspace.id} className="p-4">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <h2 className="text-sm font-semibold">{workspace.name}</h2>
                  <ul className="selectable mt-1.5 space-y-0.5">
                    {workspace.roots.map((root) => (
                      <li
                        key={root}
                        className="truncate font-mono text-xs text-[var(--color-ink-muted)]"
                        title={root}
                      >
                        {root}
                      </li>
                    ))}
                  </ul>
                </div>
                <div className="flex shrink-0 flex-col gap-1.5">
                  <Button
                    size="sm"
                    variant={workspace.id === activeId ? "primary" : "secondary"}
                    onClick={() => onSelect(workspace.id)}
                  >
                    {workspace.id === activeId ? "Selected" : "Use this"}
                  </Button>
                  <Button
                    size="sm"
                    variant="quiet"
                    onClick={() => {
                      void onAddFolder(workspace.id);
                    }}
                  >
                    Add folder
                  </Button>
                </div>
              </div>
            </Card>
          ))}
        </ul>

        <Card className="mt-5 p-4">
          <h2 className="text-sm font-semibold">New workspace</h2>
          <div className="mt-3">
            <label htmlFor="workspace-name" className="text-xs text-[var(--color-ink-muted)]">
              Name
            </label>
            <input
              id="workspace-name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Contracts"
              className="selectable mt-1 w-full rounded-md border border-[var(--color-line-strong)] bg-[var(--color-surface)] px-3 py-1.5 text-sm outline-none focus:border-[var(--color-accent)]"
            />
          </div>

          {roots.length > 0 ? (
            <ul className="selectable mt-3 space-y-0.5">
              {roots.map((root) => (
                <li key={root} className="truncate font-mono text-xs text-[var(--color-ink-muted)]">
                  {root}
                </li>
              ))}
            </ul>
          ) : null}

          <div className="mt-3 flex items-center gap-2">
            <Button
              onClick={() => {
                void pick();
              }}
            >
              Choose a folder
            </Button>
            <Button
              variant="primary"
              disabled={roots.length === 0 || busy}
              onClick={() => {
                void create();
              }}
            >
              {busy ? "Creating…" : "Create workspace"}
            </Button>
          </div>

          {error ? (
            <div className="mt-3">
              <ErrorNotice message={error} />
            </div>
          ) : null}
        </Card>
      </div>
    </div>
  );
}
