import { useCallback, useEffect, useMemo, useState } from "react";
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
import { mergePaths } from "./lib/attachments";
import { Sidebar, type View } from "./components/Sidebar";
import { Conversation as ConversationView } from "./components/Conversation";
import { Composer } from "./components/Composer";
import { ArtifactPanel } from "./components/ArtifactPanel";
import { Connections } from "./components/Connections";
import { Workspaces } from "./components/Workspaces";
import { SettingsView } from "./components/Settings";
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

  const renameConversation = useCallback(
    async (id: string, title: string) => {
      try {
        await ipc.renameConversation(id, title);
        await refreshConversations();
      } catch (cause) {
        reportError(cause);
      }
    },
    [refreshConversations, reportError],
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
      setAttachments((current) => mergePaths(current, picked));
    } else if (typeof picked === "string") {
      setAttachments((current) => mergePaths(current, [picked]));
    }
  }, []);

  const attachFolder = useCallback(async () => {
    const folder = await pickFolder();
    if (folder) {
      setAttachments((current) => mergePaths(current, [folder]));
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
        onRenameConversation={renameConversation}
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
