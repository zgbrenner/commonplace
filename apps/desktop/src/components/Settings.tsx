// Moved verbatim out of App.tsx as a mechanical step toward the planned feature-folder split — no logic changes.
import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { z } from "zod";
import * as ipc from "../lib/ipc";
import { CommonspaceError } from "../lib/ipc";
import { enableNotifications, NOTIFICATIONS_SETTING } from "../lib/notify";
import { Button, ErrorNotice } from "./primitives";

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

type NotificationsNote =
  | { kind: "info"; message: string }
  | { kind: "error"; message: string; recovery?: string | undefined };

function NotificationsSection() {
  const [enabled, setEnabled] = useState(false);
  const [note, setNote] = useState<NotificationsNote>();

  useEffect(() => {
    // Off until asked for: a stored value that is missing, false, or
    // unreadable all mean the same thing, so Commonspace never asks the
    // operating system for a permission nobody requested.
    void ipc
      .getSetting(NOTIFICATIONS_SETTING, z.boolean())
      .then((stored) => setEnabled(stored === true))
      .catch(() => {
        setEnabled(false);
      });
  }, []);

  const choose = async (next: boolean) => {
    setNote(undefined);
    if (next && !(await enableNotifications())) {
      // The system said no, so nothing is stored: a `true` here would leave
      // the toggle promising notifications that can never arrive.
      setEnabled(false);
      setNote({
        kind: "info",
        message:
          "Your system declined notifications for Commonspace, so this stayed off. You can allow them in your system notification settings, then turn this on again.",
      });
      return;
    }

    setEnabled(next);
    try {
      await ipc.setSetting(NOTIFICATIONS_SETTING, next);
    } catch (cause) {
      // Unlike the theme, this preference has no effect until it is stored,
      // so a failed write has to be said out loud.
      setEnabled(!next);
      if (cause instanceof CommonspaceError) {
        setNote({
          kind: "error",
          message: cause.message,
          ...(cause.recovery ? { recovery: cause.recovery } : {}),
        });
      } else {
        setNote({
          kind: "error",
          message: cause instanceof Error ? cause.message : String(cause),
        });
      }
    }
  };

  return (
    <section className="mt-6">
      <h2 className="text-sm font-semibold">Notifications</h2>
      <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">
        Commonspace can send one desktop notification when a task finishes, saying what happened
        and how many files changed. It only sends when the Commonspace window is not in front —
        never while you are watching the task run.
      </p>

      <fieldset className="mt-2">
        <legend className="sr-only">Notify me when a task finishes</legend>
        <div className="flex gap-2">
          <Button
            variant={enabled ? "primary" : "secondary"}
            size="sm"
            onClick={() => void choose(true)}
            aria-pressed={enabled}
          >
            On
          </Button>
          <Button
            variant={enabled ? "secondary" : "primary"}
            size="sm"
            onClick={() => void choose(false)}
            aria-pressed={!enabled}
          >
            Off
          </Button>
        </div>
      </fieldset>

      {note?.kind === "info" ? (
        <p role="status" className="mt-2 text-sm text-[var(--color-ink-muted)]">
          {note.message}
        </p>
      ) : note?.kind === "error" ? (
        <div className="mt-2">
          <ErrorNotice message={note.message} recovery={note.recovery} />
        </div>
      ) : null}
    </section>
  );
}

export function SettingsView() {
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

        <NotificationsSection />

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
