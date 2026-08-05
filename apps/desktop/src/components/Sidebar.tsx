import type { Conversation, Workspace } from "@commonspace/protocol";
import { Button } from "./primitives";

export type View = "task" | "workspaces" | "skills" | "connections" | "settings";

interface SidebarProps {
  view: View;
  onNavigate: (view: View) => void;
  conversations: Conversation[];
  activeConversationId: string | undefined;
  onOpenConversation: (id: string) => void;
  onNewTask: () => void;
  workspace: Workspace | undefined;
  connectionsNeedAttention: boolean;
}

/** Left navigation: the whole application in one column. */
export function Sidebar({
  view,
  onNavigate,
  conversations,
  activeConversationId,
  onOpenConversation,
  onNewTask,
  workspace,
  connectionsNeedAttention,
}: SidebarProps) {
  return (
    <nav
      aria-label="Main"
      className="flex w-64 shrink-0 flex-col border-r border-[var(--color-line)] bg-[var(--color-surface-sunken)]"
    >
      <div className="flex items-center gap-2 px-4 pt-4 pb-3">
        <Mark />
        <span className="text-sm font-semibold tracking-tight">Commonspace</span>
      </div>

      <div className="px-3 pb-2">
        <Button variant="primary" className="w-full" onClick={onNewTask}>
          New task
        </Button>
      </div>

      {workspace ? (
        <p className="px-4 pb-2 text-xs text-[var(--color-ink-faint)]">
          Working in <span className="text-[var(--color-ink-muted)]">{workspace.name}</span>
        </p>
      ) : null}

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pt-1">
        <h2 className="px-2 pt-2 pb-1 text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
          Recent
        </h2>
        {conversations.length === 0 ? (
          <p className="px-2 py-1 text-xs text-[var(--color-ink-faint)]">Nothing yet.</p>
        ) : (
          <ul className="space-y-0.5">
            {conversations.map((conversation) => {
              const active = view === "task" && conversation.id === activeConversationId;
              return (
                <li key={conversation.id}>
                  <button
                    type="button"
                    onClick={() => onOpenConversation(conversation.id)}
                    aria-current={active ? "page" : undefined}
                    className={`w-full truncate rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
                      active
                        ? "bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
                        : "text-[var(--color-ink-muted)] hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-ink)]"
                    }`}
                  >
                    {conversation.title || "Untitled task"}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <ul className="space-y-0.5 border-t border-[var(--color-line)] p-2">
        <NavItem
          label="Workspaces"
          active={view === "workspaces"}
          onClick={() => onNavigate("workspaces")}
        />
        <NavItem
          label="Skills"
          active={view === "skills"}
          onClick={() => onNavigate("skills")}
        />
        <NavItem
          label="Connections"
          active={view === "connections"}
          onClick={() => onNavigate("connections")}
          badge={connectionsNeedAttention ? "Needs setup" : undefined}
        />
        <NavItem
          label="Settings"
          active={view === "settings"}
          onClick={() => onNavigate("settings")}
        />
      </ul>
    </nav>
  );
}

function NavItem({
  label,
  active,
  onClick,
  badge,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
  badge?: string | undefined;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        aria-current={active ? "page" : undefined}
        className={`flex w-full items-center justify-between rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
          active
            ? "bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
            : "text-[var(--color-ink-muted)] hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-ink)]"
        }`}
      >
        {label}
        {badge ? (
          <span className="rounded-full bg-[var(--color-warn-soft)] px-1.5 py-0.5 text-[0.6875rem] font-medium text-[var(--color-warn)]">
            {badge}
          </span>
        ) : null}
      </button>
    </li>
  );
}

/**
 * The Commonspace mark: two overlapping rectangles — a shared worktable seen
 * from above, with one surface laid across another. Deliberately geometric
 * and unlike any provider's mark.
 */
function Mark() {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 20 20"
      fill="none"
      aria-hidden="true"
      className="shrink-0"
    >
      <rect
        x="1.25"
        y="4.75"
        width="12"
        height="10"
        rx="2"
        stroke="var(--color-ink-muted)"
        strokeWidth="1.5"
      />
      <rect
        x="6.75"
        y="1.25"
        width="12"
        height="10"
        rx="2"
        fill="var(--color-surface-sunken)"
        stroke="var(--color-accent)"
        strokeWidth="1.5"
      />
    </svg>
  );
}
