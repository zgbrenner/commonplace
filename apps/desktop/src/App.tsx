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
import { CommonspaceError, type PlanDecision, type TaskInfo } from "./lib/ipc";
import {
  applyEvent,
  emptyConversationState,
  isExecutionEvent,
  type ConversationState,
} from "./lib/activity";
import {
  aggregateArtifacts,
  deriveOutcome,
  isAwaitingPlanDecision,
  newestTask,
  planGates,
  replayConversationState,
  type ReplayTask,
  type TaskArtifacts,
  type TaskOutcome,
} from "./lib/replay";
import { mergePaths } from "./lib/attachments";
import { Sidebar, type View } from "./components/Sidebar";
import { Conversation as ConversationView } from "./components/Conversation";
import { Composer } from "./components/Composer";
import { ArtifactPanel } from "./components/ArtifactPanel";
import { Connections } from "./components/Connections";
import { Projects } from "./components/Projects";
import { Onboarding } from "./components/Onboarding";
import { SettingsView } from "./components/Settings";
import { EmptyState, ErrorNotice } from "./components/primitives";

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
  /** Set when `live` was rebuilt from a persisted task rather than a stream. */
  const [replayTask, setReplayTask] = useState<TaskInfo | undefined>();
  /** Artifacts per task, oldest task first — older outputs stay reachable. */
  const [artifactGroups, setArtifactGroups] = useState<TaskArtifacts[]>([]);
  /** True after the user pressed Stop on the current live task. */
  const [stopped, setStopped] = useState(false);
  /**
   * Live-stream mirror of "the plan is waiting on the user": set when a
   * gating plan arrives on the channel, cleared by execution evidence, a
   * decision, or leaving the task. The replay path ignores this flag and
   * uses the pure derivation over the persisted task instead.
   */
  const [awaitingPlanLive, setAwaitingPlanLive] = useState(false);
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
    if (event.type === "plan.created" || event.type === "plan.updated") {
      // A gating plan parks the task until the user answers; a harmless one
      // auto-proceeds on the backend with no gap.
      setAwaitingPlanLive(planGates(event.plan));
    } else if (isExecutionEvent(event)) {
      setAwaitingPlanLive(false);
    }
  }, []);

  const submit = useCallback(
    async (prompt: string) => {
      if (!activeWorkspaceId) {
        setView("projects");
        return;
      }
      // Capture before the Composer clears its list on send.
      const pendingAttachments = attachments;
      setError(undefined);
      setLive(emptyConversationState());
      setReplayTask(undefined);
      setStopped(false);
      setAwaitingPlanLive(false);
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

      // Continue the provider's own session only when everything lines up:
      // there is one, it belongs to the selected provider, and that provider
      // supports resuming. Anything else — including a failed lookup —
      // degrades silently to a fresh session.
      let resume: string | undefined;
      if (conversationId) {
        try {
          const session = await ipc.resumableSession(conversationId);
          const connection = connections.find((c) => c.provider === provider);
          if (
            session &&
            session.provider === provider &&
            connection?.capabilities.supports_resume
          ) {
            resume = session.provider_session_id;
          }
        } catch {
          // No resume — the follow-up still works, it just starts fresh.
        }
      }

      try {
        const started = await ipc.startTask(
          {
            conversationId,
            workspaceId: activeWorkspaceId,
            provider: provider as Connection["provider"],
            prompt,
            model: model === "default" ? undefined : model,
            resume,
            // Structured attachment paths — the backend records and
            // discloses them; the prompt text never embeds paths.
            attachments: pendingAttachments,
          },
          onEvent,
        );
        setTaskId(started.taskId);
        setConversationId(started.conversationId);
        setAttachments([]);
        void refreshConversations();
      } catch (cause) {
        setRunning(false);
        reportError(cause);
      }
    },
    [
      activeWorkspaceId,
      attachments,
      connections,
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
    setStopped(true);
    void ipc.cancelTask(taskId).catch(reportError);
  }, [taskId, reportError]);

  /**
   * Answer the plan the newest task is parked on. Resolves true when the
   * backend accepted the decision, false when it failed (the card uses this
   * to hand a rejected revision back for another try).
   */
  const onPlanDecision = useCallback(
    async (decision: PlanDecision): Promise<boolean> => {
      if (!taskId) return false;
      const priorReplay = replayTask;
      setError(undefined);
      if (decision.kind === "start") {
        // Execution events continue on the channel the original startTask
        // call opened — nothing to recreate here.
        setAwaitingPlanLive(false);
        setReplayTask(undefined);
        setRunning(true);
      } else if (decision.kind === "cancel") {
        setAwaitingPlanLive(false);
        setRunning(false);
        setStopped(true);
        setReplayTask((task) => (task ? { ...task, state: "cancelled" } : task));
      }
      // "revise" changes nothing up front: the card shows its quiet
      // "Updating the plan…" line and a plan.updated event re-renders it.
      try {
        await ipc.resolvePlanDecision(taskId, decision);
        return true;
      } catch (cause) {
        reportError(cause);
        if (decision.kind !== "revise") {
          // Put the unanswered card back so the user can try again.
          setReplayTask(priorReplay);
          setAwaitingPlanLive(!priorReplay);
          setRunning(false);
          setStopped(false);
        }
        return false;
      }
    },
    [taskId, replayTask, reportError],
  );

  const openConversation = useCallback(
    async (id: string) => {
      setView("task");
      setConversationId(id);
      setLive(emptyConversationState());
      setTaskId(undefined);
      setRunning(false);
      setStopped(false);
      setAwaitingPlanLive(false);
      setReplayTask(undefined);
      setArtifactGroups([]);
      try {
        const [loadedMessages, tasks] = await Promise.all([
          ipc.listMessages(id),
          ipc.listTasks(id),
        ]);
        setMessages(loadedMessages);

        // Artifacts from every task in the conversation, oldest first, so
        // outputs of earlier tasks stay reachable from the panel while each
        // stays grouped under the task that made it (undo scope).
        const ordered = [...tasks].sort((a, b) => a.created_at.localeCompare(b.created_at));
        const groups = await Promise.all(
          ordered.map(async (task) => ({
            taskId: task.id,
            artifacts: await ipc.listTaskArtifacts(task.id),
          })),
        );
        setArtifactGroups(groups);

        // Replay the newest task's event stream through the same reducer
        // the live path uses, so the reopened view matches what was shown.
        const newest = newestTask(tasks);
        if (newest) {
          setTaskId(newest.id);
          const events = await ipc.listTaskEvents(newest.id);
          const replayed = replayConversationState(events, loadedMessages);
          // The task row's stored plan is the durable copy; prefer it over
          // the one the event fold reconstructed so an unanswered plan card
          // survives even if the event stream is incomplete.
          setLive(newest.plan ? { ...replayed, plan: newest.plan } : replayed);
          setReplayTask(newest);
        }
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
    setStopped(false);
    setAwaitingPlanLive(false);
    setReplayTask(undefined);
    setArtifactGroups([]);
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

  const openArtifact = useCallback(
    (artifact: Artifact) => {
      void ipc.openArtifact(artifact.task_id, artifact.id).catch(reportError);
    },
    [reportError],
  );

  const revealArtifact = useCallback(
    (artifact: Artifact) => {
      void ipc.revealArtifact(artifact.task_id, artifact.id).catch(reportError);
    },
    [reportError],
  );

  const undoWholeTask = useCallback(async (): Promise<OperationResult[]> => {
    const target = replayTask?.id ?? taskId;
    if (!activeWorkspaceId || !target) return [];
    try {
      const results = await ipc.undoTask(activeWorkspaceId, target);
      try {
        // Refresh what the panel and card show for this task.
        const refreshed = await ipc.listTaskArtifacts(target);
        setArtifactGroups((groups) =>
          groups.some((group) => group.taskId === target)
            ? groups.map((group) =>
                group.taskId === target ? { taskId: target, artifacts: refreshed } : group,
              )
            : [...groups, { taskId: target, artifacts: refreshed }],
        );
        if (!replayTask) {
          setLive((state) => ({ ...state, artifacts: refreshed }));
        }
      } catch {
        // The undo itself succeeded; a stale panel is not worth an error.
      }
      return results;
    } catch (cause) {
      return [
        {
          success: false,
          created: [],
          modified: [],
          backups: [],
          warnings: [],
          validation: {
            outcome: "failed",
            detail: cause instanceof Error ? cause.message : String(cause),
          },
          user_summary: "Commonspace couldn't undo this task.",
        },
      ];
    }
  }, [activeWorkspaceId, replayTask, taskId]);

  /* ------------------------------------------------------- derived state */

  const panelArtifacts = useMemo(() => {
    const groups = [...artifactGroups];
    // During a live task the stream is the source of truth; on replay the
    // stored per-task lists already include the newest task.
    if (!replayTask && taskId && live.artifacts.length > 0) {
      groups.push({ taskId, artifacts: live.artifacts });
    }
    return aggregateArtifacts(groups);
  }, [artifactGroups, replayTask, taskId, live.artifacts]);

  /**
   * Is the newest task parked on its plan? Replay answers with the pure
   * derivation over the persisted task (its stored state and plan); the
   * live stream answers with the flag the channel events maintain.
   */
  const awaitingPlanApproval = useMemo(() => {
    if (replayTask) return isAwaitingPlanDecision(replayTask.state, live.plan, live);
    return awaitingPlanLive;
  }, [replayTask, live, awaitingPlanLive]);

  const outcome = useMemo<TaskOutcome | undefined>(() => {
    // While the plan card is asking the question, the outcome card stays
    // out of the way — both at once would be double UI for one decision.
    if (awaitingPlanApproval) return undefined;
    if (running) return undefined;
    if (replayTask) {
      const derived = deriveOutcome(replayTask, live, panelArtifacts);
      return derived.kind === "none" ? undefined : derived;
    }
    if (!taskId) return undefined;
    if (!live.finished && !stopped) return undefined;
    // A task that just finished on screen has no stored row in hand, so the
    // card derives from the stream instead — same component either way.
    const finishedTask: ReplayTask = {
      id: taskId,
      state: !live.finished ? "cancelled" : live.error ? "failed" : "completed",
      summary: live.summary ?? null,
      error_message: live.error?.message ?? null,
    };
    const derived = deriveOutcome(finishedTask, live, panelArtifacts);
    return derived.kind === "none" ? undefined : derived;
  }, [awaitingPlanApproval, running, replayTask, taskId, live, stopped, panelArtifacts]);

  /* ---------------------------------------------------------------- view */

  const connectionsNeedAttention = usableConnections.length === 0;
  const composerDisabled =
    running || awaitingPlanApproval || !activeWorkspaceId || usableConnections.length === 0;
  const composerReason = !activeWorkspaceId
    ? "Choose a folder in Projects to get started."
    : usableConnections.length === 0
      ? "Connect an agent in Connections to get started."
      : awaitingPlanApproval
        ? "Answer the plan above to continue, or cancel it."
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
        ) : view === "projects" ? (
          <Projects
            projects={workspaces}
            activeId={activeWorkspaceId}
            onSelect={(id) => {
              setActiveWorkspaceId(id);
              setView("task");
            }}
            onCreate={async (name, roots) => {
              await ipc.createWorkspace(name, roots);
              await refreshWorkspaces();
            }}
            onAddFolder={async (projectId) => {
              const folder = await pickFolder();
              if (folder) {
                await ipc.addWorkspaceFolder(projectId, folder);
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
        ) : !activeWorkspaceId || usableConnections.length === 0 ? (
          // Both prerequisites are the same conversation with a new person,
          // so they share one screen: whichever is missing is the next step,
          // and the other stays visible rather than appearing out of nowhere
          // once the first is done.
          <Onboarding
            hasProject={Boolean(activeWorkspaceId)}
            hasConnection={usableConnections.length > 0}
            onChooseFolder={() => setView("projects")}
            onConnectAgent={() => setView("connections")}
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
              outcome={outcome}
              awaitingPlanApproval={awaitingPlanApproval}
              onPlanDecision={onPlanDecision}
              onAnswerPermission={answerPermission}
              onCancel={cancel}
              onOpenArtifact={openArtifact}
              onRevealArtifact={revealArtifact}
              onUndoArtifact={undoArtifact}
              onUndoTask={undoWholeTask}
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

      {view === "task" && panelArtifacts.length > 0 ? (
        <ArtifactPanel
          artifacts={panelArtifacts}
          onOpen={openArtifact}
          onReveal={revealArtifact}
          onUndo={undoArtifact}
        />
      ) : null}
    </div>
  );
}
