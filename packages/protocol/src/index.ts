/**
 * The Commonspace IPC contract, mirrored from the Rust `serde` models in
 * `crates/commonspace-core`.
 *
 * Every value crossing the Tauri boundary is validated here with Zod before
 * the UI touches it: the backend is trusted, but a shape mismatch is a bug we
 * want to see immediately rather than as a downstream `undefined`.
 *
 * When a Rust type changes, change it here in the same commit. The parity
 * test in `crates/commonspace-core` serializes representative values and the
 * frontend test validates them against these schemas.
 */
import { z } from "zod";

/* ------------------------------------------------------------------ ids */

export const idSchema = z.string().min(1);

/* -------------------------------------------------------------- provider */

export const providerIdSchema = z.enum([
  "claude_code",
  "codex_cli",
  "gemini_cli",
  "open_code",
  "api_compatible",
  "local_model",
]);
export type ProviderId = z.infer<typeof providerIdSchema>;

export const installStatusSchema = z.discriminatedUnion("status", [
  z.object({ status: z.literal("installed"), version: z.string(), path: z.string() }),
  z.object({ status: z.literal("not_installed") }),
  z.object({ status: z.literal("broken"), detail: z.string() }),
]);
export type InstallStatus = z.infer<typeof installStatusSchema>;

export const authStatusSchema = z.discriminatedUnion("status", [
  z.object({ status: z.literal("not_installed") }),
  z.object({ status: z.literal("signed_out") }),
  z.object({ status: z.literal("subscription"), plan_hint: z.string().nullish() }),
  z.object({ status: z.literal("api_key") }),
  z.object({ status: z.literal("local_model") }),
  z.object({ status: z.literal("error"), detail: z.string() }),
]);
export type AuthStatus = z.infer<typeof authStatusSchema>;

export const healthCheckSchema = z.object({
  name: z.string(),
  passed: z.boolean(),
  detail: z.string().nullish(),
});

export const healthReportSchema = z.object({
  healthy: z.boolean(),
  checks: z.array(healthCheckSchema),
});
export type HealthReport = z.infer<typeof healthReportSchema>;

export const adapterCapabilitiesSchema = z.object({
  models: z.array(z.string()),
  supports_resume: z.boolean(),
  attachment_types: z.array(z.string()),
  context_tokens: z.number().nullish(),
  supports_permission_bridge: z.boolean(),
});

/** One row of the Connections screen. */
export const connectionSchema = z.object({
  provider: providerIdSchema,
  display_name: z.string(),
  install: installStatusSchema,
  auth: authStatusSchema,
  capabilities: adapterCapabilitiesSchema,
  /** Plain-language billing explanation shown under the status. */
  billing_note: z.string(),
  /** The official sign-in command, shown verbatim so nothing is hidden. */
  sign_in_command: z.string(),
  sign_in_explanation: z.string(),
});
export type Connection = z.infer<typeof connectionSchema>;

/* ------------------------------------------------------------ permissions */

export const operationClassSchema = z.enum([
  "read",
  "create",
  "modify",
  "rename",
  "move",
  "delete",
  "execute",
  "install",
  "network_fetch",
  "upload",
  "send",
  "publish",
  "secret",
]);
export type OperationClass = z.infer<typeof operationClassSchema>;

export const riskLevelSchema = z.enum(["low", "medium", "high"]);
export type RiskLevel = z.infer<typeof riskLevelSchema>;

export const permissionRequestSchema = z.object({
  id: idSchema,
  task_id: idSchema,
  session_id: idSchema.nullish(),
  operation: operationClassSchema,
  summary: z.string(),
  paths: z.array(z.string()),
  items: z.array(z.string()).default([]),
  risk: riskLevelSchema,
  irreversible: z.boolean(),
  requested_at: z.string(),
});
export type PermissionRequest = z.infer<typeof permissionRequestSchema>;

export const decisionScopeSchema = z.enum(["once", "task", "workspace"]);
export type DecisionScope = z.infer<typeof decisionScopeSchema>;

export type PermissionDecision =
  | { decision: "approve"; scope: DecisionScope }
  | { decision: "deny" };

/* ---------------------------------------------------------------- plans */

export const planStepSchema = z.object({
  title: z.string(),
  detail: z.string().nullish(),
});

export const taskPlanSchema = z.object({
  steps: z.array(planStepSchema),
  paths_accessed: z.array(z.string()),
  paths_likely_modified: z.array(z.string()),
  external_services: z.array(z.string()),
  consequential_actions: z.array(z.string()),
  deliverables: z.array(z.string()),
  requires_approval: z.boolean(),
});
export type TaskPlan = z.infer<typeof taskPlanSchema>;

/* ------------------------------------------------------------ artifacts */

export const artifactKindSchema = z.enum([
  "docx",
  "xlsx",
  "pptx",
  "pdf",
  "markdown",
  "text",
  "image",
  "code_diff",
  "other",
]);
export type ArtifactKind = z.infer<typeof artifactKindSchema>;

export const artifactSchema = z.object({
  id: idSchema,
  task_id: idSchema,
  kind: artifactKindSchema,
  path: z.string(),
  name: z.string(),
  modified_existing: z.boolean(),
  backup_path: z.string().nullish(),
  file_operation_id: idSchema.nullish(),
  change_summary: z.string().nullish(),
  created_at: z.string(),
});
export type Artifact = z.infer<typeof artifactSchema>;

/* --------------------------------------------------------------- tasks */

export const taskStateSchema = z.enum([
  "draft",
  "planning",
  "awaiting_approval",
  "running",
  "paused",
  "completed",
  "failed",
  "cancelled",
  "rolled_back",
]);
export type TaskState = z.infer<typeof taskStateSchema>;

export const toolStatusSchema = z.enum(["succeeded", "failed", "denied", "cancelled"]);
export type ToolStatus = z.infer<typeof toolStatusSchema>;

export const agentErrorSchema = z.object({
  code: z.string(),
  message: z.string(),
  recovery: z.string().nullish(),
  transient: z.boolean(),
});
export type AgentError = z.infer<typeof agentErrorSchema>;

export const usageSchema = z.object({
  input_tokens: z.number().nullish(),
  output_tokens: z.number().nullish(),
});

/* ---------------------------------------------------- normalized events */

export const agentEventSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("message.started"),
    message_id: idSchema,
    role: z.enum(["user", "assistant"]),
  }),
  z.object({ type: z.literal("message.delta"), message_id: idSchema, text: z.string() }),
  z.object({ type: z.literal("reasoning.summary"), text: z.string() }),
  z.object({ type: z.literal("plan.created"), plan: taskPlanSchema }),
  z.object({ type: z.literal("plan.updated"), plan: taskPlanSchema }),
  z.object({
    type: z.literal("tool.requested"),
    call_id: idSchema,
    tool: z.string(),
    title: z.string(),
    paths: z.array(z.string()).default([]),
  }),
  z.object({
    type: z.literal("tool.started"),
    call_id: idSchema,
    title: z.string(),
    detail: z.string().nullish(),
  }),
  z.object({ type: z.literal("tool.progress"), call_id: idSchema, detail: z.string() }),
  z.object({
    type: z.literal("tool.completed"),
    call_id: idSchema,
    status: toolStatusSchema,
    summary: z.string().nullish(),
  }),
  z.object({ type: z.literal("permission.requested"), request: permissionRequestSchema }),
  z.object({ type: z.literal("artifact.created"), artifact: artifactSchema }),
  z.object({ type: z.literal("artifact.modified"), artifact: artifactSchema }),
  z.object({ type: z.literal("warning"), message: z.string() }),
  z.object({ type: z.literal("error"), error: agentErrorSchema }),
  z.object({
    type: z.literal("task.completed"),
    summary: z.string(),
    usage: usageSchema.nullish(),
  }),
]);
export type AgentEvent = z.infer<typeof agentEventSchema>;

/* ------------------------------------------------------- operation result */

export const validationOutcomeSchema = z.discriminatedUnion("outcome", [
  z.object({ outcome: z.literal("passed") }),
  z.object({ outcome: z.literal("failed"), detail: z.string() }),
  z.object({ outcome: z.literal("not_applicable") }),
]);

export const operationResultSchema = z.object({
  success: z.boolean(),
  created: z.array(z.string()).default([]),
  modified: z.array(z.string()).default([]),
  backups: z.array(z.string()).default([]),
  warnings: z.array(z.string()).default([]),
  validation: validationOutcomeSchema,
  user_summary: z.string(),
  diagnostics: z.string().nullish(),
});
export type OperationResult = z.infer<typeof operationResultSchema>;

/* ------------------------------------------------- workspaces and history */

export const workspaceSchema = z.object({
  id: idSchema,
  name: z.string(),
  roots: z.array(z.string()),
});
export type Workspace = z.infer<typeof workspaceSchema>;

export const conversationSchema = z.object({
  id: idSchema,
  workspace_id: idSchema.nullish(),
  title: z.string(),
  updated_at: z.string(),
});
export type Conversation = z.infer<typeof conversationSchema>;

export const messageSchema = z.object({
  id: idSchema,
  conversation_id: idSchema,
  role: z.enum(["user", "assistant"]),
  content: z.string(),
  created_at: z.string(),
});
export type Message = z.infer<typeof messageSchema>;

export const taskSummarySchema = z.object({
  id: idSchema,
  conversation_id: idSchema,
  workspace_id: idSchema.nullish(),
  provider: providerIdSchema,
  state: taskStateSchema,
  prompt: z.string(),
  plan: taskPlanSchema.nullish(),
  summary: z.string().nullish(),
});
export type TaskSummary = z.infer<typeof taskSummarySchema>;

/* --------------------------------------------------------------- helpers */

/**
 * Parse a value from the backend, throwing a descriptive error on mismatch.
 * Used at every IPC boundary — never trust the shape, even from our own Rust.
 */
export function parseFromBackend<T>(
  schema: z.ZodType<T, z.ZodTypeDef, unknown>,
  value: unknown,
  context: string,
): T {
  const result = schema.safeParse(value);
  if (!result.success) {
    throw new Error(`${context}: unexpected data from Commonspace — ${result.error.message}`);
  }
  return result.data;
}
