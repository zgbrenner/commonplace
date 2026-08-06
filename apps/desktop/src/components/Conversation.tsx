import { useEffect, useRef } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Message } from "@commonspace/protocol";
import type { ActivityItem, ConversationState } from "../lib/activity";
import { permissionHeadline } from "../lib/activity";
import { openExternalUrl } from "../lib/ipc";
import { Button, Card, ErrorNotice, StatusPill, TechnicalDetails } from "./primitives";

interface ConversationProps {
  messages: Message[];
  live: ConversationState;
  running: boolean;
  onAnswerPermission: (approve: boolean, scope?: "once" | "task" | "workspace") => void;
  onCancel: () => void;
}

/** The conversation column: messages, progress, approvals, result. */
export function Conversation({
  messages,
  live,
  running,
  onAnswerPermission,
  onCancel,
}: ConversationProps) {
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [messages.length, live.assistantText, live.activity.length, live.pendingPermission]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex max-w-3xl flex-col gap-5 px-6 py-6">
        {messages.map((message) => (
          <MessageBubble key={message.id} role={message.role} content={message.content} />
        ))}

        {live.assistantText ? (
          <MessageBubble role="assistant" content={live.assistantText} />
        ) : null}

        {live.plan ? <PlanCard plan={live.plan} /> : null}

        {live.activity.length > 0 ? (
          <ActivityTimeline items={live.activity} reasoning={live.reasoning} />
        ) : null}

        {live.pendingPermission ? (
          <PermissionCard
            request={live.pendingPermission}
            onAnswer={onAnswerPermission}
          />
        ) : null}

        {live.warnings.map((warning, index) => (
          <p
            key={`${warning}-${index}`}
            className="text-sm text-[var(--color-warn)]"
            role="status"
          >
            <span aria-hidden="true" className="mr-1.5">
              ⚠
            </span>
            {warning}
          </p>
        ))}

        {live.error ? (
          <ErrorNotice message={live.error.message} recovery={live.error.recovery} />
        ) : null}

        {running ? (
          <div className="flex items-center gap-3">
            <span
              className="text-sm text-[var(--color-ink-muted)]"
              role="status"
              aria-live="polite"
            >
              Working…
            </span>
            <Button size="sm" variant="quiet" onClick={onCancel}>
              Stop
            </Button>
          </div>
        ) : null}

        <div ref={endRef} />
      </div>
    </div>
  );
}

/**
 * Overrides for the Markdown renderer, defined once at module level so the
 * object identity is stable across renders.
 */
const markdownComponents: Components = {
  // Links never navigate the webview — that would replace the whole app with
  // the destination page. They open in the user's default browser via the
  // backend instead, and the title attribute shows where a link goes on
  // hover, since the webview has no status bar.
  a: ({ href, children }) => (
    <a
      href={href}
      title={href}
      onClick={(event) => {
        event.preventDefault();
        if (href) {
          void openExternalUrl(href).catch(() => {
            // A link that fails to open is not worth an error banner.
          });
        }
      }}
    >
      {children}
    </a>
  ),
  // Wide tables scroll inside their own wrapper rather than stretching the
  // conversation column.
  table: ({ children }) => (
    <div className="markdown-table-scroll">
      <table>{children}</table>
    </div>
  ),
};

function MessageBubble({ role, content }: { role: "user" | "assistant"; content: string }) {
  if (role === "user") {
    // User text stays plain: the person typed it literally, and rendering
    // their asterisks or brackets as markup would be surprising.
    return (
      <div className="flex justify-end">
        <div className="selectable max-w-[85%] rounded-[var(--radius-card)] bg-[var(--color-accent-soft)] px-4 py-2.5 text-sm whitespace-pre-wrap text-[var(--color-ink)]">
          <span className="sr-only">You said:</span>
          {content}
        </div>
      </div>
    );
  }

  // Assistant replies render as Markdown. Raw HTML stays disabled
  // (react-markdown's default; no rehype-raw) — deliberately, because model
  // output is untrusted. During streaming this component re-parses the whole
  // text on every delta; at chat-message sizes that is cheap, so it is kept
  // intentionally simple rather than incrementally parsed.
  return (
    <div>
      <div className="selectable markdown max-w-full text-sm text-[var(--color-ink)]">
        <span className="sr-only">Commonspace replied:</span>
        <Markdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
          {content}
        </Markdown>
      </div>
    </div>
  );
}

function PlanCard({ plan }: { plan: ConversationState["plan"] }) {
  if (!plan) return null;
  return (
    <Card className="p-4">
      <h3 className="text-sm font-semibold">Plan</h3>
      <ol className="mt-2 space-y-1.5">
        {plan.steps.map((step, index) => (
          <li key={`${step.title}-${index}`} className="flex gap-2.5 text-sm">
            <span className="text-[var(--color-ink-faint)] tabular-nums">{index + 1}.</span>
            <span>
              {step.title}
              {step.detail ? (
                <span className="block text-[var(--color-ink-muted)]">{step.detail}</span>
              ) : null}
            </span>
          </li>
        ))}
      </ol>
      {plan.deliverables.length > 0 ? (
        <p className="mt-3 text-sm text-[var(--color-ink-muted)]">
          You&apos;ll get: {plan.deliverables.join(", ")}
        </p>
      ) : null}
      {plan.consequential_actions.length > 0 ? (
        <p className="mt-2 text-sm text-[var(--color-warn)]">
          Needs your approval: {plan.consequential_actions.join(", ")}
        </p>
      ) : null}
    </Card>
  );
}

/**
 * The activity timeline. One readable line per step, with the raw
 * provider-facing information folded away under Technical details.
 */
function ActivityTimeline({
  items,
  reasoning,
}: {
  items: ActivityItem[];
  reasoning: string[];
}) {
  return (
    <Card className="overflow-hidden">
      <ul className="divide-y divide-[var(--color-line)]">
        {items.map((item) => (
          <li key={item.id} className="flex items-start gap-3 px-4 py-2.5">
            <ActivityGlyph status={item.status} />
            <div className="min-w-0 flex-1">
              <p className="text-sm text-[var(--color-ink)]">{item.title}</p>
              {item.detail ? (
                <p className="truncate text-xs text-[var(--color-ink-muted)]">{item.detail}</p>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
      {reasoning.length > 0 ? (
        <div className="px-4 pb-3">
          <TechnicalDetails label="What the agent was thinking">
            {reasoning.join("\n\n")}
          </TechnicalDetails>
        </div>
      ) : null}
    </Card>
  );
}

/** Status is conveyed by glyph plus accessible text, not colour alone. */
function ActivityGlyph({ status }: { status: ActivityItem["status"] }) {
  const map = {
    running: { glyph: "◐", label: "In progress", color: "text-[var(--color-ink-muted)]" },
    done: { glyph: "✓", label: "Done", color: "text-[var(--color-ok)]" },
    failed: { glyph: "✕", label: "Failed", color: "text-[var(--color-danger)]" },
    denied: { glyph: "⊘", label: "Not allowed", color: "text-[var(--color-warn)]" },
    info: { glyph: "•", label: "Note", color: "text-[var(--color-ink-faint)]" },
  } as const;
  const { glyph, label, color } = map[status];
  return (
    <span className={`mt-0.5 shrink-0 text-sm ${color}`}>
      <span aria-hidden="true">{glyph}</span>
      <span className="sr-only">{label}</span>
    </span>
  );
}

/**
 * A permission request. Shows the resolved destination paths — what the user
 * is actually approving — and warns explicitly when an action is
 * irreversible.
 */
function PermissionCard({
  request,
  onAnswer,
}: {
  request: NonNullable<ConversationState["pendingPermission"]>;
  onAnswer: (approve: boolean, scope?: "once" | "task" | "workspace") => void;
}) {
  return (
    <Card
      as="section"
      className="border-[var(--color-accent)] p-4"
      aria-labelledby="permission-heading"
    >
      <div className="flex items-start justify-between gap-3">
        <h3 id="permission-heading" className="text-sm font-semibold">
          {permissionHeadline(request)}
        </h3>
        <StatusPill
          tone={request.risk === "high" ? "danger" : request.risk === "medium" ? "warn" : "neutral"}
          glyph={request.risk === "high" ? "!" : undefined}
        >
          {request.risk === "high"
            ? "Higher risk"
            : request.risk === "medium"
              ? "Changes files"
              : "Low risk"}
        </StatusPill>
      </div>

      <p className="mt-1.5 text-sm text-[var(--color-ink-muted)]">{request.summary}</p>

      {request.irreversible ? (
        <p className="mt-2 text-sm font-medium text-[var(--color-danger)]">
          <span aria-hidden="true" className="mr-1.5">
            ⚠
          </span>
          This cannot be undone.
        </p>
      ) : null}

      {request.paths.length > 0 ? (
        <ul className="selectable mt-3 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface-sunken)] p-2.5 font-mono text-xs text-[var(--color-ink-muted)]">
          {request.paths.map((path) => (
            <li key={path} className="truncate" title={path}>
              {path}
            </li>
          ))}
        </ul>
      ) : null}

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button variant="primary" onClick={() => onAnswer(true, "once")}>
          Allow once
        </Button>
        <Button onClick={() => onAnswer(true, "task")}>Allow for this task</Button>
        <Button variant="quiet" onClick={() => onAnswer(false)}>
          Don&apos;t allow
        </Button>
      </div>
      <p className="mt-2 text-xs text-[var(--color-ink-faint)]">
        Declining is always safe — the task continues without this step.
      </p>
    </Card>
  );
}
