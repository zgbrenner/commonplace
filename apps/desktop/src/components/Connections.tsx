import { useState } from "react";
import type { Connection, HealthReport } from "@commonspace/protocol";
import { Button, Card, StatusPill, TechnicalDetails } from "./primitives";

interface ConnectionsProps {
  connections: Connection[];
  onRefresh: () => void;
  onCheckHealth: (provider: Connection["provider"]) => Promise<HealthReport>;
  refreshing: boolean;
}

/**
 * The Connections screen. Every state here is the truth reported by the
 * provider's own tooling — Commonspace never guesses, and never implies a
 * subscription works where the official tooling doesn't support it.
 */
export function Connections({
  connections,
  onRefresh,
  onCheckHealth,
  refreshing,
}: ConnectionsProps) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-3xl px-6 py-6">
        <header className="flex items-start justify-between gap-4">
          <div>
            <h1 className="text-lg font-semibold">Connections</h1>
            <p className="mt-1 text-sm text-[var(--color-ink-muted)]">
              Commonspace works through the official tools these providers publish. Signing in
              happens in their tool, and your credentials stay there — Commonspace never stores
              or copies them.
            </p>
          </div>
          <Button onClick={onRefresh} disabled={refreshing}>
            {refreshing ? "Checking…" : "Check again"}
          </Button>
        </header>

        <ul className="mt-5 space-y-3">
          {connections.map((connection) => (
            <ConnectionRow
              key={connection.provider}
              connection={connection}
              onCheckHealth={onCheckHealth}
            />
          ))}
        </ul>
      </div>
    </div>
  );
}

function ConnectionRow({
  connection,
  onCheckHealth,
}: {
  connection: Connection;
  onCheckHealth: (provider: Connection["provider"]) => Promise<HealthReport>;
}) {
  const [health, setHealth] = useState<HealthReport | undefined>();
  const status = describeStatus(connection);

  return (
    <Card as="li" className="p-4">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">{connection.display_name}</h2>
          <p className="mt-1 text-sm text-[var(--color-ink-muted)]">{connection.billing_note}</p>
        </div>
        <StatusPill tone={status.tone} glyph={status.glyph}>
          {status.label}
        </StatusPill>
      </div>

      {connection.install.status === "installed" ? (
        <p className="mt-2 text-xs text-[var(--color-ink-faint)]">
          Version {connection.install.version}
        </p>
      ) : null}

      {needsSetup(connection) ? (
        <div className="mt-3 rounded-md bg-[var(--color-surface-sunken)] p-3">
          <p className="text-sm text-[var(--color-ink-muted)]">
            {connection.sign_in_explanation}
          </p>
          <p className="mt-2 text-xs text-[var(--color-ink-faint)]">
            Run this in a terminal to sign in:
          </p>
          <code className="selectable mt-1 block rounded bg-[var(--color-surface)] px-2 py-1.5 font-mono text-xs text-[var(--color-ink)]">
            {connection.sign_in_command}
          </code>
        </div>
      ) : null}

      <div className="mt-3 flex items-center gap-2">
        <Button
          size="sm"
          variant="quiet"
          onClick={() => {
            void onCheckHealth(connection.provider).then(setHealth);
          }}
        >
          Run a check
        </Button>
      </div>

      {health ? (
        <ul className="mt-2 space-y-1">
          {health.checks.map((check) => (
            <li key={check.name} className="text-xs text-[var(--color-ink-muted)]">
              <span aria-hidden="true" className="mr-1.5">
                {check.passed ? "✓" : "✕"}
              </span>
              <span className="sr-only">{check.passed ? "Passed:" : "Failed:"}</span>
              {check.name}
              {check.detail ? ` — ${check.detail}` : ""}
            </li>
          ))}
        </ul>
      ) : null}

      <TechnicalDetails>
        {JSON.stringify(
          {
            install: connection.install,
            auth: connection.auth,
            capabilities: connection.capabilities,
          },
          null,
          2,
        )}
      </TechnicalDetails>
    </Card>
  );
}

function needsSetup(connection: Connection): boolean {
  return (
    connection.auth.status === "signed_out" ||
    connection.auth.status === "not_installed" ||
    connection.install.status !== "installed"
  );
}

function describeStatus(connection: Connection): {
  label: string;
  tone: "ok" | "warn" | "danger" | "neutral" | "accent";
  glyph: string;
} {
  if (connection.install.status === "not_installed") {
    return { label: "Not installed", tone: "neutral", glyph: "○" };
  }
  if (connection.install.status === "broken") {
    return { label: "Needs attention", tone: "danger", glyph: "!" };
  }
  switch (connection.auth.status) {
    case "subscription":
      return {
        label: connection.auth.plan_hint
          ? `Connected · ${connection.auth.plan_hint}`
          : "Connected · subscription",
        tone: "ok",
        glyph: "✓",
      };
    case "api_key":
      return { label: "Connected · API billing", tone: "ok", glyph: "✓" };
    case "local_model":
      return { label: "Running locally", tone: "ok", glyph: "✓" };
    case "signed_out":
      return { label: "Sign-in required", tone: "warn", glyph: "→" };
    case "error":
      return { label: "Couldn't check", tone: "danger", glyph: "!" };
    case "not_installed":
      return { label: "Not installed", tone: "neutral", glyph: "○" };
    default:
      return { label: "Unknown", tone: "neutral", glyph: "?" };
  }
}
