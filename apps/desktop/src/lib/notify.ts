/**
 * Desktop notifications for finished tasks.
 *
 * A task can run for many minutes, so the person is often looking at
 * something else when it ends. One notification then is useful; a
 * notification for something they are already watching is noise, so nothing
 * is sent while the Commonspace window has focus.
 *
 * The wording is built by `completionNotification`, which is pure and
 * testable. Everything that touches the operating system is lazy and
 * failure-tolerant: outside a Tauri host (unit tests, a browser preview)
 * these functions do nothing rather than break.
 */
import { z } from "zod";

/** Settings key for whether finished tasks send a desktop notification. */
export const NOTIFICATIONS_SETTING = "notifications.enabled";

/**
 * Turn notifications on, asking the operating system for permission if it
 * has not been granted. Resolves to whether they are actually usable now.
 *
 * Never throws: a refusal, a missing plugin, or no Tauri host at all all
 * resolve to `false`, which the caller shows as "still off".
 */
export async function enableNotifications(): Promise<boolean> {
  try {
    const notification = await import("@tauri-apps/plugin-notification");
    if (await notification.isPermissionGranted()) return true;
    return (await notification.requestPermission()) === "granted";
  } catch {
    return false;
  }
}

/**
 * Send the completion notification, but only when the window is not focused.
 *
 * Never throws and never rejects: a notification is the least important thing
 * happening at the end of a task, and must not be able to take the task down
 * with it. Anything unexpected — no Tauri host, a revoked permission, a
 * settings read that fails — ends in silence.
 */
export async function notifyTaskFinished(message: {
  title: string;
  body: string;
}): Promise<void> {
  try {
    // Focus first: it is the cheapest check and the most common reason to
    // stay quiet.
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    if (await getCurrentWindow().isFocused()) return;

    // The operating system's permission is not the person's consent — the
    // Settings toggle is, and it is off until they turn it on.
    const ipc = await import("./ipc");
    if ((await ipc.getSetting(NOTIFICATIONS_SETTING, z.boolean())) !== true) return;

    const notification = await import("@tauri-apps/plugin-notification");
    if (!(await notification.isPermissionGranted())) return;
    notification.sendNotification({ title: message.title, body: message.body });
  } catch {
    // Silence is the right failure mode here.
  }
}

/**
 * How much of a task summary a notification carries. Notification bodies are
 * truncated by the operating system anyway, and an unreadable wall of text is
 * worse than a short line plus the app.
 */
const SUMMARY_LIMIT = 140;

/** What a finished task should say, in plain language. */
export function completionNotification(input: {
  projectName: string | undefined;
  state: "completed" | "failed" | "cancelled";
  summary: string | undefined;
  changedFiles: number;
}): { title: string; body: string } {
  const project = text(input.projectName);
  const summary = clamp(text(input.summary), SUMMARY_LIMIT);
  const changed = Math.max(0, Math.trunc(input.changedFiles));

  if (input.state === "failed") {
    // Lead with the outcome — that it did not finish is the thing to know
    // before any explanation of why.
    return {
      title: project ? `Commonspace didn't finish in ${project}` : "Commonspace didn't finish",
      body: sentences(
        "This task didn't finish.",
        summary ?? (changed === 0 ? "No reason was reported." : undefined),
        changed > 0 ? `${files(changed)} changed before it stopped.` : undefined,
      ),
    };
  }

  if (input.state === "cancelled") {
    return {
      title: project ? `Task stopped in ${project}` : "Task stopped",
      body: sentences(
        "You stopped this task.",
        changed > 0 ? `${files(changed)} changed before it stopped.` : "No files were changed.",
        summary,
      ),
    };
  }

  return {
    title: project ? `Commonspace finished in ${project}` : "Commonspace finished",
    body: sentences(
      changed > 0 ? `${files(changed)} changed.` : "No files were changed.",
      summary,
    ),
  };
}

/** `undefined` for anything that is missing or only whitespace. */
function text(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : undefined;
}

function files(count: number): string {
  return count === 1 ? "1 file" : `${count} files`;
}

function sentences(...parts: (string | undefined)[]): string {
  return parts.filter((part): part is string => part !== undefined).join(" ");
}

/**
 * Cut long text at a word boundary and mark the cut with the visible ellipsis
 * used everywhere else in Commonspace, so it reads as shortened rather than
 * as text that mysteriously stops.
 */
function clamp(value: string | undefined, limit: number): string | undefined {
  if (value === undefined || value.length <= limit) return value;
  const head = value.slice(0, limit);
  const lastSpace = head.lastIndexOf(" ");
  const cut = lastSpace > limit / 2 ? head.slice(0, lastSpace) : head;
  return `${cut.replace(/[\s.,;:]+$/, "")}…`;
}
