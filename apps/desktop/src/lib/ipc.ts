/**
 * The typed IPC boundary.
 *
 * Every call goes through here, and every response is validated with Zod
 * before it reaches a component. Errors from Rust arrive as
 * `{ message, recovery? }` and are re-thrown as a `CommonspaceError` the UI
 * can render with a useful recovery action instead of a stack trace.
 */
import { invoke, Channel } from "@tauri-apps/api/core";
import {
  agentEventSchema,
  artifactSchema,
  connectionSchema,
  conversationSchema,
  healthReportSchema,
  messageSchema,
  operationResultSchema,
  parseFromBackend,
  workspaceSchema,
  type AgentEvent,
  type Artifact,
  type Connection,
  type Conversation,
  type DecisionScope,
  type HealthReport,
  type Message,
  type OperationResult,
  type ProviderId,
  type Workspace,
} from "@commonspace/protocol";
import { z } from "zod";

export class CommonspaceError extends Error {
  readonly recovery: string | undefined;
  constructor(message: string, recovery?: string) {
    super(message);
    this.name = "CommonspaceError";
    this.recovery = recovery;
  }
}

const backendErrorSchema = z.object({
  message: z.string(),
  recovery: z.string().nullish(),
});

// The third type parameter is `unknown`: several schemas accept a narrower
// input than they produce (Rust omits empty arrays, Zod fills them back in),
// so the input and output types legitimately differ.
async function call<T>(
  command: string,
  args: Record<string, unknown>,
  schema: z.ZodType<T, z.ZodTypeDef, unknown>,
): Promise<T> {
  try {
    const raw = await invoke(command, args);
    return parseFromBackend(schema, raw, command);
  } catch (error) {
    const parsed = backendErrorSchema.safeParse(error);
    if (parsed.success) {
      throw new CommonspaceError(parsed.data.message, parsed.data.recovery ?? undefined);
    }
    throw new CommonspaceError(
      typeof error === "string" ? error : "Something went wrong inside Commonspace.",
    );
  }
}

/* ------------------------------------------------------------ connections */

export const listConnections = () =>
  call("list_connections", {}, z.array(connectionSchema));

export const providerHealth = (provider: ProviderId) =>
  call("provider_health", { provider }, healthReportSchema);

export const signInInstructions = (provider: ProviderId) =>
  call(
    "sign_in_instructions",
    { provider },
    z.object({ command: z.string(), explanation: z.string() }),
  );

/* ------------------------------------------------------------- workspaces */

export const listWorkspaces = () => call("list_workspaces", {}, z.array(workspaceSchema));

export const createWorkspace = (name: string, roots: string[]) =>
  call("create_workspace", { name, roots }, workspaceSchema);

export const addWorkspaceFolder = (workspaceId: string, root: string) =>
  call("add_workspace_folder", { workspaceId, root }, z.void().or(z.null()));

/* ---------------------------------------------------------- conversations */

export const listConversations = (limit?: number) =>
  call("list_conversations", { limit }, z.array(conversationSchema));

export const listMessages = (conversationId: string) =>
  call("list_messages", { conversationId }, z.array(messageSchema));

export const renameConversation = (conversationId: string, title: string) =>
  call("rename_conversation", { conversationId, title }, z.void().or(z.null()));

/* ----------------------------------------------------------------- search */

/**
 * One full-text search hit: either a conversation matched by title or a
 * message matched by content. The snippet is plain text with … ellipses —
 * no HTML — so it can be rendered directly.
 *
 * Exported (unlike most local schemas here) so the contract test can parse
 * fixture responses against it.
 */
export const searchResultSchema = z.object({
  kind: z.enum(["conversation", "message"]),
  conversation_id: z.string(),
  title: z.string(),
  snippet: z.string(),
  created_at: z.string(),
});
export type SearchResult = z.infer<typeof searchResultSchema>;

export const searchHistory = (query: string, limit?: number) =>
  call("search_history", { query, limit }, z.array(searchResultSchema));

/* ---------------------------------------------------------------- opening */

/**
 * Open a link in the user's default browser. The webview itself must never
 * navigate — the backend validates and hands the URL to the OS.
 */
export const openExternalUrl = (url: string) =>
  call("open_external_url", { url }, z.void().or(z.null()));

/* ------------------------------------------------------------------ tasks */

/** A persisted task, as needed to replay a conversation's history. */
export const taskInfoSchema = z.object({
  id: z.string(),
  conversation_id: z.string(),
  provider: z.string(),
  state: z.enum([
    "draft",
    "planning",
    "awaiting_approval",
    "running",
    "paused",
    "completed",
    "failed",
    "cancelled",
    "rolled_back",
  ]),
  summary: z.string().nullish(),
  error_message: z.string().nullish(),
  created_at: z.string(),
  updated_at: z.string(),
});
export type TaskInfo = z.infer<typeof taskInfoSchema>;

export const listTasks = (conversationId: string): Promise<TaskInfo[]> =>
  call("list_tasks", { conversationId }, z.array(taskInfoSchema));

/**
 * The provider session a follow-up message can continue, when the
 * conversation's most recent task left one behind and the provider supports
 * resuming. Null when there is nothing to continue.
 */
export const resumableSessionSchema = z
  .object({
    provider: z.string(),
    provider_session_id: z.string(),
  })
  .nullable();
export type ResumableSession = z.infer<typeof resumableSessionSchema>;

export const resumableSession = (conversationId: string): Promise<ResumableSession> =>
  call("resumable_session", { conversationId }, resumableSessionSchema);

/** Undo every file operation a task performed, newest first. */
export const undoTask = (workspaceId: string, taskId: string): Promise<OperationResult[]> =>
  call("undo_task", { workspaceId, taskId }, z.array(operationResultSchema));

/* ------------------------------------------------------------ attachments */

/** A persisted attachment: metadata only, never file contents. */
export const attachmentInfoSchema = z.object({
  id: z.string(),
  conversation_id: z.string(),
  task_id: z.string().nullish(),
  path: z.string(),
  kind: z.enum(["file", "folder"]),
  size_bytes: z.number().nullish(),
  modified_at: z.string().nullish(),
  content_hash: z.string().nullish(),
  in_workspace: z.boolean(),
  created_at: z.string(),
});
export type AttachmentInfo = z.infer<typeof attachmentInfoSchema>;

export const listConversationAttachments = (
  conversationId: string,
): Promise<AttachmentInfo[]> =>
  call("list_conversation_attachments", { conversationId }, z.array(attachmentInfoSchema));

export interface StartTaskArgs {
  conversationId?: string | undefined;
  workspaceId: string;
  provider: ProviderId;
  prompt: string;
  model?: string | undefined;
  resume?: string | undefined;
  /**
   * Attached file/folder paths. The backend canonicalizes, records, and
   * discloses these — the prompt text itself no longer carries paths.
   */
  attachments?: string[] | undefined;
}

const startedTaskSchema = z.object({
  task_id: z.string(),
  conversation_id: z.string(),
});

/**
 * Start a task. `onEvent` receives normalized events over a dedicated Tauri
 * channel, which preserves ordering under high-frequency token streaming.
 */
export async function startTask(
  args: StartTaskArgs,
  onEvent: (event: AgentEvent) => void,
): Promise<{ taskId: string; conversationId: string }> {
  const channel = new Channel<unknown>();
  channel.onmessage = (raw) => {
    const parsed = agentEventSchema.safeParse(raw);
    if (parsed.success) {
      onEvent(parsed.data);
    } else {
      // An unrecognized event is a protocol drift bug, not a reason to break
      // the session: log it and keep the timeline running.
      console.warn("Commonspace received an unrecognized event", raw, parsed.error);
    }
  };

  const result = await call(
    "start_task",
    {
      args: {
        conversation_id: args.conversationId ?? null,
        workspace_id: args.workspaceId,
        provider: args.provider,
        prompt: args.prompt,
        model: args.model ?? null,
        resume: args.resume ?? null,
        attachments: args.attachments ?? [],
      },
      onEvent: channel,
    },
    startedTaskSchema,
  );
  return { taskId: result.task_id, conversationId: result.conversation_id };
}

export const cancelTask = (taskId: string) =>
  call("cancel_task", { taskId }, z.void().or(z.null()));

export const answerPermission = (requestId: string, approve: boolean, scope?: DecisionScope) =>
  call("answer_permission", { requestId, approve, scope: scope ?? null }, z.boolean());

export const listTaskArtifacts = (taskId: string): Promise<Artifact[]> =>
  call("list_task_artifacts", { taskId }, z.array(artifactSchema));

export const listTaskEvents = (taskId: string, afterSeq?: number): Promise<AgentEvent[]> =>
  call("list_task_events", { taskId, afterSeq: afterSeq ?? null }, z.array(agentEventSchema));

export const undoFileOperation = (
  workspaceId: string,
  fileOperationId: string,
): Promise<OperationResult> =>
  call("undo_file_operation", { workspaceId, fileOperationId }, operationResultSchema);

export const openArtifact = (taskId: string, artifactId: string) =>
  call("open_artifact", { taskId, artifactId }, z.void().or(z.null()));

export const revealArtifact = (taskId: string, artifactId: string) =>
  call("reveal_artifact", { taskId, artifactId }, z.void().or(z.null()));

/* ---------------------------------------------------------------- updates */

const updateCheckSchema = z.object({
  current_version: z.string(),
  latest_version: z.string().nullish(),
  available: z.boolean(),
  notes: z.string().nullish(),
  in_place: z.boolean(),
  release_url: z.string(),
});
export type UpdateCheck = z.infer<typeof updateCheckSchema>;

export const checkForUpdate = (): Promise<UpdateCheck> =>
  call("check_for_update", {}, updateCheckSchema);

const installProgressSchema = z.discriminatedUnion("phase", [
  z.object({
    phase: z.literal("downloading"),
    received: z.number(),
    total: z.number().nullish(),
  }),
  z.object({ phase: z.literal("installing") }),
]);
export type InstallProgress = z.infer<typeof installProgressSchema>;

/**
 * Download and install an update in place. On success the app restarts
 * itself, so this promise only ever settles with an error.
 */
export async function installUpdate(
  onProgress: (progress: InstallProgress) => void,
): Promise<void> {
  const channel = new Channel<unknown>();
  channel.onmessage = (raw) => {
    const parsed = installProgressSchema.safeParse(raw);
    if (parsed.success) {
      onProgress(parsed.data);
    }
  };
  await call("install_update", { onProgress: channel }, z.void().or(z.null()));
}

export const openReleasePage = (url?: string) =>
  call("open_release_page", { url: url ?? null }, z.void().or(z.null()));

/* --------------------------------------------------------------- settings */

export const getSetting = <T>(key: string, schema: z.ZodType<T, z.ZodTypeDef, unknown>) =>
  call("get_setting", { key }, schema.nullish());

export const setSetting = (key: string, value: unknown) =>
  call("set_setting", { key, value }, z.void().or(z.null()));

export type {
  AgentEvent,
  Artifact,
  Connection,
  Conversation,
  HealthReport,
  Message,
  OperationResult,
  Workspace,
};
