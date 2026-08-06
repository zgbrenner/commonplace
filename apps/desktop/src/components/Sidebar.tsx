import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import type { Conversation, Workspace } from "@commonspace/protocol";
import * as ipc from "../lib/ipc";
import { createLatestGuard } from "../lib/search";
import { Button } from "./primitives";

export type View = "task" | "workspaces" | "skills" | "connections" | "settings";

/** How long typing must pause before a search request is sent. */
const SEARCH_DEBOUNCE_MS = 200;

interface SidebarProps {
  view: View;
  onNavigate: (view: View) => void;
  conversations: Conversation[];
  activeConversationId: string | undefined;
  onOpenConversation: (id: string) => void;
  onNewTask: () => void;
  onRenameConversation: (id: string, title: string) => Promise<void>;
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
  onRenameConversation,
  workspace,
  connectionsNeedAttention,
}: SidebarProps) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<ipc.SearchResult[] | undefined>();
  // Responses can arrive out of order; only the newest query's answer counts.
  const guardRef = useRef(createLatestGuard<string>());

  useEffect(() => {
    const guard = guardRef.current;
    const trimmed = query.trim();
    if (trimmed === "") {
      // Invalidate anything still in flight, then fall back to the recents.
      guard.begin("");
      setResults(undefined);
      return;
    }
    const timer = setTimeout(() => {
      guard.begin(trimmed);
      ipc.searchHistory(trimmed, 25).then(
        (found) => {
          if (guard.isCurrent(trimmed)) setResults(found);
        },
        () => {
          // A failed search reads as "nothing found" rather than an error
          // banner — the recents list is one keystroke away.
          if (guard.isCurrent(trimmed)) setResults([]);
        },
      );
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  const searching = query.trim().length > 0;

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

      <div className="px-3 pb-1">
        <label htmlFor="history-search" className="sr-only">
          Search history
        </label>
        <input
          id="history-search"
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") setQuery("");
          }}
          placeholder="Search history"
          className="selectable w-full rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] px-2 py-1.5 text-sm text-[var(--color-ink)] outline-none placeholder:text-[var(--color-ink-faint)] focus:border-[var(--color-accent)]"
        />
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pt-1">
        {searching ? (
          <SearchResults results={results} onOpenConversation={onOpenConversation} />
        ) : (
          <>
            <h2 className="px-2 pt-2 pb-1 text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
              Recent
            </h2>
            {conversations.length === 0 ? (
              <p className="px-2 py-1 text-xs text-[var(--color-ink-faint)]">Nothing yet.</p>
            ) : (
              <ul className="space-y-0.5">
                {conversations.map((conversation) => (
                  <ConversationRow
                    key={conversation.id}
                    conversation={conversation}
                    active={view === "task" && conversation.id === activeConversationId}
                    onOpen={() => onOpenConversation(conversation.id)}
                    onRename={(title) => onRenameConversation(conversation.id, title)}
                  />
                ))}
              </ul>
            )}
          </>
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

/** Search hits shown in place of the recents while a query is active. */
function SearchResults({
  results,
  onOpenConversation,
}: {
  results: ipc.SearchResult[] | undefined;
  onOpenConversation: (id: string) => void;
}) {
  return (
    <>
      <h2 className="px-2 pt-2 pb-1 text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
        Results
      </h2>
      {results === undefined ? null : results.length === 0 ? (
        <p className="px-2 py-1 text-xs text-[var(--color-ink-faint)]">Nothing found.</p>
      ) : (
        <ul className="space-y-0.5">
          {results.map((result, index) => (
            // A conversation can match several times (title plus messages),
            // so the key includes the position.
            <li key={`${result.conversation_id}-${result.kind}-${index}`}>
              <button
                type="button"
                onClick={() => onOpenConversation(result.conversation_id)}
                className="w-full rounded-md px-2 py-1.5 text-left text-sm text-[var(--color-ink-muted)] transition-colors hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-ink)]"
              >
                <span className="block truncate">{result.title || "Untitled task"}</span>
                {result.snippet ? (
                  <span className="block truncate text-xs text-[var(--color-ink-faint)]">
                    {result.snippet}
                  </span>
                ) : null}
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

/** One recent conversation: open on click, rename via the pencil. */
function ConversationRow({
  conversation,
  active,
  onOpen,
  onRename,
}: {
  conversation: Conversation;
  active: boolean;
  onOpen: () => void;
  onRename: (title: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  // Enter and Escape both end editing before blur fires; this flag keeps
  // that blur from committing a second time.
  const settled = useRef(false);

  const startEditing = () => {
    setDraft(conversation.title);
    settled.current = false;
    setEditing(true);
  };

  const commit = () => {
    const title = draft.trim();
    setEditing(false);
    // An empty or unchanged title means "never mind" (the backend rejects
    // empty titles as well).
    if (title === "" || title === conversation.title) return;
    void onRename(title);
  };

  const onEditKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") {
      settled.current = true;
      commit();
    } else if (event.key === "Escape") {
      settled.current = true;
      setEditing(false);
    }
  };

  if (editing) {
    return (
      <li>
        <input
          // The input replaces the row the user just activated, so moving
          // focus into it is the expected continuation, not a focus steal.
          autoFocus
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onEditKeyDown}
          onBlur={() => {
            if (!settled.current) commit();
          }}
          aria-label="Conversation title"
          className="selectable w-full rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] px-2 py-1 text-sm text-[var(--color-ink)] outline-none"
        />
      </li>
    );
  }

  return (
    <li className="group flex items-center gap-0.5">
      <button
        type="button"
        onClick={onOpen}
        aria-current={active ? "page" : undefined}
        className={`min-w-0 flex-1 truncate rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
          active
            ? "bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
            : "text-[var(--color-ink-muted)] hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-ink)]"
        }`}
      >
        {conversation.title || "Untitled task"}
      </button>
      <button
        type="button"
        onClick={startEditing}
        aria-label="Rename conversation"
        className="shrink-0 rounded-md p-1 text-[var(--color-ink-faint)] opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100 hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-ink)] focus:opacity-100"
      >
        <span aria-hidden="true">✎</span>
      </button>
    </li>
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
