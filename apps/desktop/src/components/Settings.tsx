// Moved verbatim out of App.tsx as a mechanical step toward the planned feature-folder split — no logic changes.
import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import * as ipc from "../lib/ipc";
import { CommonspaceError } from "../lib/ipc";
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
