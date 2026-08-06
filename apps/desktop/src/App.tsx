import { useCallback, useEffect, useMemo, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type {
  AgentEvent,
  Artifact,
  Connection,
  Conversation,
  Message,
  OperationResult,
  Workspace,
} from "@commonspace/protocol";
import * as ipc from "./lib/ipc";
import { CommonspaceError } from "./lib/ipc";
import { applyEvent, emptyConversationState, type ConversationState } from "./lib/activity";
import { Sidebar, type View } from "./components/Sidebar";
import { Conversation as ConversationView } from "./components/Conversation";
import { Composer } from "./components/Composer";
import { ArtifactPanel } from "./components/ArtifactPanel";
import { Connections } from "./components/Connections";
import { Workspaces } from "./components/Workspaces";
import { Button, EmptyState, ErrorNotice } from "./components/primitives";

export function App() {
  const [view, setView] = useState<View>("task");
  const [connections, setConnections] = useState<Connection[]>([]);
  const [refreshingConnections, setRefreshingConnections] = useState(false);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [activeWorkspaceId, setActiveWorkspaceId] = useState<string | undefined>();
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [conversationId, setConversationId] = useState<string | undefined>();
  const [messages, setMessages] = useState<Message[]>([]);
  const [live, setLive] = useState<ConversationState>(emptyConversationState);
  const [taskId, setTaskId] = useState<string | undefined>();
  const [running, setRunning] = useState(false);
  const [provider, setProvider] = useState("claude_code");
  const [model, setModel] = useState("default");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [error, setError] = useState<{ message: string; recovery?: string } | undefined>();

  const workspace = useMemo(
    () => workspaces.find((w) => w.id === activeWorkspaceId),
    [workspaces, activeWorkspaceId],
  );

  const usableConnections = useMemo(
    () =>
      connections.filter(
        (c) =>
          c.auth.status === "subscription" ||
          c.auth.status === "api_key" ||
          c.auth.status === "local_model",
      ),
    [connections],
  );

  const reportError = useCallback((cause: unknown) => {
    if (cause instanceof CommonspaceError) {
      setError({ message: cause.message, ...(cause.recovery ? { recovery: cause.recovery } : {}) });
    } else {
      setError({ message: cause instanceof Error ? cause.message : String(cause) });
    }
  }, []);

  const refreshConnections = useCallback(async () => {
    setRefreshingConnections(true);
    try {
      const next = await ipc.listConnections();
      setConnections(next);
      const usable = next.find(
        (c) =>
          c.auth.status === "subscription" ||
          c.auth.status === "api_key" ||
          c.auth.status === "local_model",
      );
      if (usable) {
        setProvider((current) =>
          next.some(
            (c) =>
              c.provider === current &&
              (c.auth.status === "subscription" ||
                c.auth.status === "api_key" ||
                c.auth.status === "local_model"),
          )
            ? current
            : usable.provider,
        );
      }
    } catch (cause) {
      reportError(cause);
    } finally {
      setRefreshingConnections(false);
    }
  }, [reportError]);

  const refreshWorkspaces = useCallback(async () => {
    try {
      const next = await ipc.listWorkspaces();
      setWorkspaces(next);
      setActiveWorkspaceId((current) => current ?? next[0]?.id);
    } catch (cause) {
      reportError(cause);
    }
  }, [reportError]);

  const refreshConversations = useCallback(async () => {
    try {
      setConversations(await ipc.listConversations(50));
    } catch (cause) {
      reportError(cause);
    }
  }, [reportError]);

  useEffect(() => {
    void refreshConnections();
    void refreshWorkspaces();
    void refreshConversations();
  }, [refreshConnections, refreshWorkspaces, refreshConversations]);

  /* ------------------------------------------------------------- actions */

  const onEvent = useCallback((event: AgentEvent) => {
    setLive((state) => applyEvent(state, event));
    if (event.type === "task.completed" || event.type === "error") {
      setRunning(false);
    }
  }, []);

  const submit = useCallback(
    async (prompt: string) => {
      if (!activeWorkspaceId) {
        setView("workspaces");
        return;
      }
      setError(undefined);
      setLive(emptyConversationState());
      setRunning(true);
      // Show the prompt immediately rather than waiting for the round trip.
      setMessages((current) => [
        ...current,
        {
          id: `local-${Date.now()}`,
          conversation_id: conversationId ?? "",
          role: "user",
          content: prompt,
          created_at: new Date().toISOString(),
        },
      ]);
      try {
        const started = await ipc.startTask(
          {
            conversationId,
            workspaceId: activeWorkspaceId,
            provider: provider as Connection["provider"],
            prompt,
            model: model === "default" ? undefined : model,
          },
          onEvent,
        );
        setTaskId(started.taskId);
        setConversationId(started.conversationId);
        void refreshConversations();
      } catch (cause) {
        setRunning(false);
        reportError(cause);
      }
    },
    [
      activeWorkspaceId,
      conversationId,
      provider,
      model,
      onEvent,
      refreshConversations,
      reportError,
    ],
  );

  const answerPermission = useCallback(
    (approve: boolean, scope?: "once" | "task" | "workspace") => {
      const request = live.pendingPermission;
      if (!request) return;
      setLive((state) => ({ ...state, pendingPermission: undefined }));
      void ipc.answerPermission(request.id, approve, scope).catch(reportError);
    },
    [live.pendingPermission, reportError],
  );

  const cancel = useCallback(() => {
    if (!taskId) return;
    setRunning(false);
    void ipc.cancelTask(taskId).catch(reportError);
  }, [taskId, reportError]);

  const openConversation = useCallback(
    async (id: string) => {
      setView("task");
      setConversationId(id);
      setLive(emptyConversationState());
      setTaskId(undefined);
      setRunning(false);
      try {
        setMessages(await ipc.listMessages(id));
      } catch (cause) {
        reportError(cause);
      }
    },
    [reportError],
  );

  const newTask = useCallback(() => {
    setView("task");
    setConversationId(undefined);
    setMessages([]);
    setLive(emptyConversationState());
    setTaskId(undefined);
    setRunning(false);
    setAttachments([]);
  }, []);

  const pickFolder = useCallback(async (): Promise<string | undefined> => {
    const picked = await openDialog({ directory: true, multiple: false });
    return typeof picked === "string" ? picked : undefined;
  }, []);

  const pickFiles = useCallback(async () => {
    const picked = await openDialog({ directory: false, multiple: true });
    if (Array.isArray(picked)) {
      setAttachments((current) => [...new Set([...current, ...picked])]);
    } else if (typeof picked === "string") {
      setAttachments((current) => [...new Set([...current, picked])]);
    }
  }, []);

  const attachFolder = useCallback(async () => {
    const folder = await pickFolder();
    if (folder) {
      setAttachments((current) => [...new Set([...current, folder])]);
    }
  }, [pickFolder]);

  const undoArtifact = useCallback(
    async (artifact: Artifact): Promise<OperationResult> => {
      if (!activeWorkspaceId || !artifact.file_operation_id) {
        return {
          success: false,
          created: [],
          modified: [],
          backups: [],
          warnings: [],
          validation: { outcome: "failed", detail: "No undo record was kept for this change." },
          user_summary: "This change can't be undone.",
        };
      }
      try {
        return await ipc.undoFileOperation(activeWorkspaceId, artifact.file_operation_id);
      } catch (cause) {
        return {
          success: false,
          created: [],
          modified: [],
          backups: [],
          warnings: [],
          validation: {
            outcome: "failed",
            detail: cause instanceof Error ? cause.message : String(cause),
          },
          user_summary: "Commonspace couldn't undo this change.",
        };
      }
    },
    [activeWorkspaceId],
  );

  /* ---------------------------------------------------------------- view */

  const connectionsNeedAttention = usableConnections.length === 0;
  const composerDisabled = running || !activeWorkspaceId || usableConnections.length === 0;
  const composerReason = !activeWorkspaceId
    ? "Choose a folder in Workspaces to get started."
    : usableConnections.length === 0
      ? "Connect an agent in Connections to get started."
      : undefined;

  return (
    <div className="flex h-full">
      <Sidebar
        view={view}
        onNavigate={setView}
        conversations={conversations}
        activeConversationId={conversationId}
        onOpenConversation={(id) => void openConversation(id)}
        onNewTask={newTask}
        workspace={workspace}
        connectionsNeedAttention={connectionsNeedAttention}
      />

      <main className="flex min-w-0 flex-1 flex-col">
        {error ? (
          <div className="px-6 pt-4">
            <ErrorNotice
              message={error.message}
              recovery={error.recovery}
              onRetry={() => setError(undefined)}
            />
          </div>
        ) : null}

        {view === "connections" ? (
          <Connections
            connections={connections}
            onRefresh={() => void refreshConnections()}
            onCheckHealth={(p) => ipc.providerHealth(p)}
            refreshing={refreshingConnections}
          />
        ) : view === "workspaces" ? (
          <Workspaces
            workspaces={workspaces}
            activeId={activeWorkspaceId}
            onSelect={(id) => {
              setActiveWorkspaceId(id);
              setView("task");
            }}
            onCreate={async (name, roots) => {
              await ipc.createWorkspace(name, roots);
              await refreshWorkspaces();
            }}
            onAddFolder={async (workspaceId) => {
              const folder = await pickFolder();
              if (folder) {
                await ipc.addWorkspaceFolder(workspaceId, folder);
                await refreshWorkspaces();
              }
            }}
            onPickFolder={pickFolder}
          />
        ) : view === "skills" ? (
          <EmptyState
            title="Reusable workflows are coming"
            description="Saved instructions for jobs you repeat — comparing contracts, turning a folder of PDFs into a spreadsheet, tidying Downloads. They'll be plain Markdown files you can read and edit."
          />
        ) : view === "settings" ? (
          <SettingsView />
        ) : !activeWorkspaceId ? (
          <EmptyState
            title="Choose a folder to work in"
            description="Commonspace only sees the folders you authorize. Pick one to get started; you can add more later."
            action={<Button variant="primary" onClick={() => setView("workspaces")}>Choose a folder</Button>}
          />
        ) : usableConnections.length === 0 ? (
          <EmptyState
            title="Connect an agent you already pay for"
            description="Commonspace runs on the official tools from Anthropic, OpenAI and others. Sign in there once and Commonspace will use it — no new account, no extra charge from us."
            action={
              <Button variant="primary" onClick={() => setView("connections")}>
                Open Connections
              </Button>
            }
          />
        ) : messages.length === 0 && !live.assistantText ? (
          <>
            <EmptyState
              title={`Ready to work in ${workspace?.name ?? "your folder"}`}
              description="Ask for something concrete: summarize these contracts, find duplicate files, turn this folder of PDFs into a spreadsheet, rename these scans by date."
            />
            <Composer
              workspace={workspace}
              connections={connections}
              provider={provider}
              onProviderChange={setProvider}
              model={model}
              onModelChange={setModel}
              attachments={attachments}
              onAttachmentsChange={setAttachments}
              onAttachFiles={() => void pickFiles()}
              onAttachFolder={() => void attachFolder()}
              onSubmit={(prompt) => void submit(prompt)}
              disabled={composerDisabled}
              disabledReason={composerReason}
            />
          </>
        ) : (
          <>
            <ConversationView
              messages={messages}
              live={live}
              running={running}
              onAnswerPermission={answerPermission}
              onCancel={cancel}
            />
            <Composer
              workspace={workspace}
              connections={connections}
              provider={provider}
              onProviderChange={setProvider}
              model={model}
              onModelChange={setModel}
              attachments={attachments}
              onAttachmentsChange={setAttachments}
              onAttachFiles={() => void pickFiles()}
              onAttachFolder={() => void attachFolder()}
              onSubmit={(prompt) => void submit(prompt)}
              disabled={composerDisabled}
              disabledReason={composerReason}
            />
          </>
        )}
      </main>

      {view === "task" && live.artifacts.length > 0 ? (
        <ArtifactPanel
          artifacts={live.artifacts}
          onOpen={(artifact) => {
            if (taskId) void ipc.openArtifact(taskId, artifact.id).catch(reportError);
          }}
          onReveal={(artifact) => {
            if (taskId) void ipc.revealArtifact(taskId, artifact.id).catch(reportError);
          }}
          onUndo={undoArtifact}
        />
      ) : null}
    </div>
  );
}

type UpdateUiState =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "current" }
  | { phase: "available"; check: ipc.UpdateCheck }
  | { phase: "installing"; note: string }
  | { phase: "failed"; message: string; recovery?: string | undefined };

function UpdatesSection() {
  const [appVersion, setAppVersion] = useState<string>();
  const [update, setUpdate] = useState<UpdateUiState>({ phase: "idle" });

  useEffect(() => {
    void getVersion()
      .then(setAppVersion)
      .catch(() => {
        // The section still works without the version label.
      });
  }, []);

  const fail = (cause: unknown) => {
    if (cause instanceof CommonspaceError) {
      setUpdate({
        phase: "failed",
        message: cause.message,
        ...(cause.recovery ? { recovery: cause.recovery } : {}),
      });
    } else {
      setUpdate({
        phase: "failed",
        message: cause instanceof Error ? cause.message : String(cause),
      });
    }
  };

  const check = async () => {
    setUpdate({ phase: "checking" });
    try {
      const result = await ipc.checkForUpdate();
      setUpdate(result.available ? { phase: "available", check: result } : { phase: "current" });
    } catch (cause) {
      fail(cause);
    }
  };

  const install = async () => {
    setUpdate({ phase: "installing", note: "Preparing the download…" });
    try {
      await ipc.installUpdate((progress) => {
        if (progress.phase === "downloading") {
          const total = progress.total ?? undefined;
          setUpdate({
            phase: "installing",
            note: total
              ? `Downloading… ${Math.min(100, Math.round((progress.received / total) * 100))}%`
              : `Downloading… ${(progress.received / (1024 * 1024)).toFixed(1)} MB so far`,
          });
        } else {
          setUpdate({
            phase: "installing",
            note: "Installing… Commonspace will restart by itself.",
          });
        }
      });
      // On success the app restarts; this line is never reached.
    } catch (cause) {
      fail(cause);
    }
  };

  const busy = update.phase === "checking" || update.phase === "installing";

  return (
    <section className="mt-6">
      <h2 className="text-sm font-semibold">Updates</h2>
      <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">
        {appVersion ? `You're running Commonspace ${appVersion}. ` : ""}
        Updates are checked only when you ask, and nothing installs without another click.
      </p>

      <div className="mt-2 flex items-center gap-2">
        <Button variant="secondary" size="sm" onClick={() => void check()} disabled={busy}>
          {update.phase === "checking" ? "Checking…" : "Check for updates"}
        </Button>
        {update.phase === "available" ? (
          update.check.in_place ? (
            <Button variant="primary" size="sm" onClick={() => void install()}>
              Download and install {update.check.latest_version ?? "the update"}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              onClick={() => void ipc.openReleasePage(update.check.release_url).catch(fail)}
            >
              Open the download page
            </Button>
          )
        ) : null}
      </div>

      {update.phase === "current" ? (
        <p className="mt-2 text-sm text-[var(--color-ink-muted)]">
          You're on the newest version.
        </p>
      ) : update.phase === "available" ? (
        <p className="mt-2 text-sm text-[var(--color-ink-muted)]">
          {update.check.latest_version
            ? `Version ${update.check.latest_version} is available.`
            : "A newer version is available."}
          {update.check.in_place
            ? " Commonspace can install it and restart."
            : " It installs like the first time: download, run, done — your conversations and settings stay."}
        </p>
      ) : update.phase === "installing" ? (
        <p className="mt-2 text-sm text-[var(--color-ink-muted)]" role="status">
          {update.note}
        </p>
      ) : update.phase === "failed" ? (
        <div className="mt-2">
          <ErrorNotice
            message={update.message}
            recovery={update.recovery}
            onRetry={() => void check()}
          />
        </div>
      ) : null}
    </section>
  );
}

function SettingsView() {
  const [theme, setTheme] = useState<string>(
    () => document.documentElement.dataset["theme"] ?? "system",
  );

  const applyTheme = (next: string) => {
    setTheme(next);
    if (next === "system") {
      delete document.documentElement.dataset["theme"];
    } else {
      document.documentElement.dataset["theme"] = next;
    }
    void ipc.setSetting("theme", next).catch(() => {
      // A failed preference write is not worth interrupting the user over;
      // the choice still applies for this session.
    });
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-3xl px-6 py-6">
        <h1 className="text-lg font-semibold">Settings</h1>

        <section className="mt-5">
          <h2 className="text-sm font-semibold">Appearance</h2>
          <fieldset className="mt-2">
            <legend className="sr-only">Theme</legend>
            <div className="flex gap-2">
              {(["system", "light", "dark"] as const).map((option) => (
                <Button
                  key={option}
                  variant={theme === option ? "primary" : "secondary"}
                  size="sm"
                  onClick={() => applyTheme(option)}
                  aria-pressed={theme === option}
                >
                  {option === "system" ? "Match system" : option === "light" ? "Light" : "Dark"}
                </Button>
              ))}
            </div>
          </fieldset>
        </section>

        <UpdatesSection />

        <section className="mt-6">
          <h2 className="text-sm font-semibold">Privacy</h2>
          <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">
            Your conversations, task history, backups and audit records stay on this computer.
            Commonspace collects no telemetry. When you run a task with a cloud provider, your
            prompt and the file contents that task reads are sent to that provider under their
            terms.
          </p>
        </section>

        <section className="mt-6">
          <h2 className="text-sm font-semibold">Safety</h2>
          <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">
            Files are backed up before they are changed or deleted, and deletions go to your
            system trash rather than disappearing. Commonspace checks the result on disk before
            reporting a change as done.
          </p>
        </section>
      </div>
    </div>
  );
}
