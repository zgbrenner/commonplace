import { useState } from "react";
import type { Workspace } from "@commonspace/protocol";
import { Button, Card, ErrorNotice } from "./primitives";

interface ProjectsProps {
  projects: Workspace[];
  activeId: string | undefined;
  onSelect: (id: string) => void;
  onCreate: (name: string, roots: string[]) => Promise<void>;
  onAddFolder: (projectId: string) => Promise<void>;
  onPickFolder: () => Promise<string | undefined>;
}

/** The name a project falls back to when someone leaves the field empty. */
const UNNAMED_PROJECT = "Project";

/**
 * Projects: the folders the agent is allowed to touch. Everything outside
 * them requires a fresh, explicit grant through the native picker.
 */
export function Projects({
  projects,
  activeId,
  onSelect,
  onCreate,
  onAddFolder,
  onPickFolder,
}: ProjectsProps) {
  const [name, setName] = useState("");
  const [roots, setRoots] = useState<string[]>([]);
  const [error, setError] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);

  const pick = async () => {
    const folder = await onPickFolder();
    if (folder && !roots.includes(folder)) {
      setRoots([...roots, folder]);
      if (!name) {
        setName(folder.split(/[/\\]/).filter(Boolean).pop() ?? UNNAMED_PROJECT);
      }
    }
  };

  const create = async () => {
    setBusy(true);
    setError(undefined);
    try {
      await onCreate(name.trim() || UNNAMED_PROJECT, roots);
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
        <h1 className="text-lg font-semibold">Projects</h1>
        <p className="mt-1 text-sm text-[var(--color-ink-muted)]">
          A project is the set of folders you&apos;ve given Commonspace permission to work in;
          anything outside them needs a fresh yes from you first.
        </p>

        <ul className="mt-5 space-y-3">
          {projects.map((project) => (
            <Card as="li" key={project.id} className="p-4">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <h2 className="text-sm font-semibold">{project.name}</h2>
                  <ul className="selectable mt-1.5 space-y-0.5">
                    {project.roots.map((root) => (
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
                    variant={project.id === activeId ? "primary" : "secondary"}
                    onClick={() => onSelect(project.id)}
                  >
                    {project.id === activeId ? "Selected" : "Use this"}
                  </Button>
                  <Button
                    size="sm"
                    variant="quiet"
                    onClick={() => {
                      void onAddFolder(project.id);
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
          <h2 className="text-sm font-semibold">New project</h2>
          <div className="mt-3">
            <label htmlFor="project-name" className="text-xs text-[var(--color-ink-muted)]">
              Name
            </label>
            <input
              id="project-name"
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
              {busy ? "Creating…" : "Create project"}
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
