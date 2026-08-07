import { useEffect, useRef, useState } from "react";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Artifact, Message, OperationResult, TaskPlan } from "@commonspace/protocol";
import type { ActivityItem, CalmProgress, ConversationState } from "../lib/activity";
import { announcement, calmProgress, permissionHeadline } from "../lib/activity";
import type { TaskOutcome as TaskOutcomeModel } from "../lib/replay";
import { openExternalUrl, type PlanDecision } from "../lib/ipc";
import { TaskOutcome } from "./TaskOutcome";
import {
  Button,
  Card,
  Disclosure,
  ErrorNotice,
  StatusPill,
  TechnicalDetails,
} from "./primitives";

interface ConversationProps {
  messages: Message[];
  live: ConversationState;
  running: boolean;
  /** The newest task's outcome, once it is terminal (live or replayed). */
  outcome: TaskOutcomeModel | undefined;
  /** True when the newest task's plan is waiting on the user's decision. */
  awaitingPlanApproval: boolean;
  /** Resolves true when the decision was accepted, false when it failed. */
  onPlanDecision: (decision: PlanDecision) => Promise<boolean>;
  onAnswerPermission: (approve: boolean, scope?: "once" | "task" | "workspace") => void;
  onCancel: () => void;
  onOpenArtifact: (artifact: Artifact) => void;
  onRevealArtifact: (artifact: Artifact) => void;
  onUndoArtifact: (artifact: Artifact) => Promise<OperationResult>;
  onUndoTask: () => Promise<OperationResult[]>;
}

/** The conversation column: messages, progress, approvals, result. */
export function Conversation({
  messages,
  live,
  running,
  outcome,
  awaitingPlanApproval,
  onPlanDecision,
  onAnswerPermission,
  onCancel,
  onOpenArtifact,
  onRevealArtifact,
  onUndoArtifact,
  onUndoTask,
}: ConversationProps) {
  const endRef = useRef<HTMLDivElement>(null);
  const elapsed = useElapsedRun(running, outcome?.taskId);

  // Follow the newest content. Every dependency here is something that adds
  // a block above the anchor or changes one's height: `live.activity` by
  // identity rather than length, because a step changing status rewrites the
  // progress card's headline and notices without the list growing.
  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [
    messages.length,
    live.assistantText,
    live.activity,
    live.plan,
    live.pendingPermission,
    running,
    awaitingPlanApproval,
    outcome,
  ]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex max-w-3xl flex-col gap-5 px-6 py-6">
        {/*
          The conversation column's only live region, and the reason the rest
          of the column is silent. It is mounted from the first render and
          only its text changes: a region added to the page at the same
          moment as its first sentence is frequently missed. `aria-atomic`
          makes each new sentence read whole rather than diffed word by word.
        */}
        <p className="sr-only" role="status" aria-live="polite" aria-atomic="true">
          {announcement(live, running, awaitingPlanApproval)}
        </p>

        {messages.map((message) => (
          <MessageBubble key={message.id} role={message.role} content={message.content} />
        ))}

        {live.assistantText ? (
          <MessageBubble role="assistant" content={live.assistantText} streaming />
        ) : null}

        {live.plan ? (
          <PlanCard
            plan={live.plan}
            awaiting={awaitingPlanApproval}
            onDecision={onPlanDecision}
          />
        ) : null}

        {live.activity.length > 0 || running ? (
          // Rendered from the moment the task starts, so pressing Send is
          // answered on screen before the first step arrives, and left in
          // place afterwards so the finished task can still be read back.
          <ProgressCard
            progress={calmProgress(live, running)}
            items={live.activity}
            reasoning={live.reasoning}
            // While a plan waits on the user nothing is working — the plan
            // card above carries the actions, so Stop stays hidden.
            canStop={running && !awaitingPlanApproval}
            onCancel={onCancel}
          />
        ) : null}

        {live.pendingPermission ? (
          <PermissionCard
            request={live.pendingPermission}
            onAnswer={onAnswerPermission}
          />
        ) : null}

        {live.warnings.map((warning, index) => (
          <p key={`${warning}-${index}`} className="text-sm text-[var(--color-warn)]">
            <span aria-hidden="true" className="mr-1.5">
              ⚠
            </span>
            <span className="sr-only">Warning: </span>
            {warning}
          </p>
        ))}

        {live.error && !outcome ? (
          // Once the outcome card exists it owns the failure presentation,
          // so the raw notice would show the same error twice.
          <ErrorNotice
            message={live.error.message}
            recovery={live.error.recovery}
            announce={false}
          />
        ) : null}

        {outcome ? (
          <TaskOutcome
            key={outcome.taskId}
            outcome={outcome}
            elapsedMs={elapsed}
            usage={live.usage}
            onOpen={onOpenArtifact}
            onReveal={onRevealArtifact}
            onUndoArtifact={onUndoArtifact}
            onUndoTask={onUndoTask}
          />
        ) : null}

        <div ref={endRef} />
      </div>
    </div>
  );
}

/**
 * How long the task on screen spent running, measured here because the event
 * stream carries no clock.
 *
 * The measurement restarts every time execution begins, so a plan parked on
 * the user's decision is not counted as work, and it stops when the outcome
 * card appears. A conversation reopened from history never ran here: its
 * outcome carries no measurement, and the card omits the line rather than
 * inventing a number.
 */
function useElapsedRun(running: boolean, outcomeTaskId: string | undefined): number | undefined {
  const startedAt = useRef<number | undefined>(undefined);
  const [measured, setMeasured] = useState<{ taskId: string; ms: number } | undefined>();

  useEffect(() => {
    if (running) startedAt.current = Date.now();
  }, [running]);

  useEffect(() => {
    const started = startedAt.current;
    if (started === undefined || outcomeTaskId === undefined) return;
    startedAt.current = undefined;
    setMeasured({ taskId: outcomeTaskId, ms: Date.now() - started });
  }, [outcomeTaskId]);

  // Keyed by task, so switching to another conversation cannot show one
  // task's duration under another task's outcome.
  return measured && measured.taskId === outcomeTaskId ? measured.ms : undefined;
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

function MessageBubble({
  role,
  content,
  streaming = false,
}: {
  role: "user" | "assistant";
  content: string;
  streaming?: boolean;
}) {
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
      <div
        // Said explicitly rather than left to the default: text arriving a
        // token at a time must never be announced as it arrives. The hidden
        // status line at the top of the column speaks for the task instead.
        aria-live={streaming ? "off" : undefined}
        className="selectable markdown max-w-full text-sm text-[var(--color-ink)]"
      >
        <span className="sr-only">Commonspace replied:</span>
        <Markdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
          {content}
        </Markdown>
      </div>
    </div>
  );
}

/**
 * The plan card. Display-only while a task is merely narrating what it will
 * do; decisional — Start / Change plan / Cancel — while the task is parked
 * waiting for the user's answer. A revision round-trips through
 * `plan.updated`, which replaces the `plan` prop and resets the card back
 * to its summary.
 */
function PlanCard({
  plan,
  awaiting,
  onDecision,
}: {
  plan: TaskPlan;
  awaiting: boolean;
  onDecision: (decision: PlanDecision) => Promise<boolean>;
}) {
  const [mode, setMode] = useState<"summary" | "editing" | "revising">("summary");
  const [feedback, setFeedback] = useState("");

  // A new plan (created or revised) always starts back at the summary.
  useEffect(() => {
    setMode("summary");
    setFeedback("");
  }, [plan]);

  const sendRevision = () => {
    const text = feedback.trim();
    if (!text) return;
    setMode("revising");
    void onDecision({ kind: "revise", feedback: text }).then((accepted) => {
      // On success stay in "revising" until plan.updated resets the card;
      // on failure hand the text back so the user can try again.
      if (!accepted) setMode("editing");
    });
  };

  return (
    <Card as="section" className="p-4">
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
      {plan.paths_likely_modified.length > 0 ? (
        <div className="mt-3">
          <h4 className="text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
            Files that may change
          </h4>
          <ul className="selectable mt-1.5 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface-sunken)] p-2.5 font-mono text-xs text-[var(--color-ink-muted)]">
            {plan.paths_likely_modified.map((path) => (
              <li key={path} className="truncate" title={path}>
                {path}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {awaiting ? (
        mode === "revising" ? (
          <p className="mt-3 text-sm text-[var(--color-ink-muted)]">Updating the plan…</p>
        ) : mode === "editing" ? (
          <div className="mt-3">
            <label htmlFor="plan-feedback" className="text-sm font-medium text-[var(--color-ink)]">
              What should be different?
            </label>
            <textarea
              id="plan-feedback"
              value={feedback}
              onChange={(event) => setFeedback(event.target.value)}
              rows={3}
              autoFocus
              className="selectable mt-1.5 w-full resize-y rounded-md border border-[var(--color-line-strong)] bg-[var(--color-surface)] px-2.5 py-1.5 text-sm text-[var(--color-ink)] outline-none placeholder:text-[var(--color-ink-faint)]"
            />
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <Button
                variant="primary"
                size="sm"
                onClick={sendRevision}
                disabled={feedback.trim().length === 0}
              >
                Send
              </Button>
              <Button size="sm" variant="quiet" onClick={() => setMode("summary")}>
                Back
              </Button>
            </div>
          </div>
        ) : (
          <>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <Button variant="primary" onClick={() => void onDecision({ kind: "start" })}>
                Start
              </Button>
              <Button onClick={() => setMode("editing")}>Change plan</Button>
              <Button variant="quiet" onClick={() => void onDecision({ kind: "cancel" })}>
                Cancel
              </Button>
            </div>
            <p className="mt-2 text-xs text-[var(--color-ink-faint)]">
              Nothing runs until you press Start.
            </p>
          </>
        )
      ) : null}
    </Card>
  );
}

/**
 * Progress, told the way a person would tell it: one sentence for what is
 * happening, a count, and anything that went wrong or was refused. The
 * step-by-step record — and the agent's reasoning — wait under Details for
 * whoever wants them.
 */
function ProgressCard({
  progress,
  items,
  reasoning,
  canStop,
  onCancel,
}: {
  progress: CalmProgress;
  items: ActivityItem[];
  reasoning: string[];
  canStop: boolean;
  onCancel: () => void;
}) {
  return (
    <Card as="section" className="p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          {/* Plain text, deliberately: this same sentence is what the column's
              one live region announces, and a second region saying it would
              double every step. */}
          <p className="text-sm text-[var(--color-ink)]">{progress.headline}</p>
          {progress.progress ? (
            <p className="mt-0.5 text-xs text-[var(--color-ink-faint)]">{progress.progress}</p>
          ) : null}
        </div>
        {canStop ? (
          <Button size="sm" variant="quiet" className="shrink-0" onClick={onCancel}>
            Stop
          </Button>
        ) : null}
      </div>

      {progress.notices.length > 0 ? (
        <ul className="mt-2.5 space-y-1">
          {progress.notices.map((notice) => (
            <li key={notice} className="text-sm text-[var(--color-warn)]">
              <span aria-hidden="true" className="mr-1.5">
                ⚠
              </span>
              {/* A failed step's notice is just its title, so the warning
                  needs a word of its own for anyone not seeing the glyph. */}
              <span className="sr-only">Needs your attention: </span>
              {notice}
            </li>
          ))}
        </ul>
      ) : null}

      {items.length > 0 || reasoning.length > 0 ? (
        <Disclosure label="Details">
          {items.length > 0 ? (
            <ul className="divide-y divide-[var(--color-line)] rounded-md border border-[var(--color-line)]">
              {items.map((item) => (
                <li key={item.id} className="flex items-start gap-3 px-3 py-2">
                  <ActivityGlyph status={item.status} />
                  <div className="min-w-0 flex-1">
                    <p className="text-sm text-[var(--color-ink)]">{item.title}</p>
                    {item.detail ? (
                      <p className="truncate text-xs text-[var(--color-ink-muted)]">
                        {item.detail}
                      </p>
                    ) : null}
                  </div>
                </li>
              ))}
            </ul>
          ) : null}
          {reasoning.length > 0 ? (
            <TechnicalDetails label="What the agent was thinking">
              {reasoning.join("\n\n")}
            </TechnicalDetails>
          ) : null}
        </Disclosure>
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
 * is actually approving — itemizes a batch so the individual operations are
 * readable, and warns explicitly when an action is irreversible.
 */
function PermissionCard({
  request,
  onAnswer,
}: {
  request: NonNullable<ConversationState["pendingPermission"]>;
  onAnswer: (approve: boolean, scope?: "once" | "task" | "workspace") => void;
}) {
  const headingRef = useRef<HTMLHeadingElement>(null);

  /*
   * A question that appears mid-stream and does not take focus is, for
   * someone driving by keyboard, a question they have to go hunting for
   * while the page grows underneath them. So focus moves to the card's
   * heading — the heading, not a button, because landing on "Allow once"
   * puts approval one stray keypress away — and returns to wherever it came
   * from once the question is answered.
   */
  useEffect(() => {
    const previous = document.activeElement;
    headingRef.current?.focus();
    return () => {
      // Only if that element is still on the page: the composer the user
      // came from survives, a button inside a card that has since been
      // replaced does not, and focusing a detached node silently drops
      // focus to the body.
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
    };
  }, [request.id]);

  return (
    <Card
      as="section"
      className="border-[var(--color-accent)] p-4"
      aria-labelledby="permission-heading"
    >
      <div className="flex items-start justify-between gap-3">
        <h3 id="permission-heading" ref={headingRef} tabIndex={-1} className="text-sm font-semibold">
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

      {request.items.length > 0 ? (
        <div className="mt-3">
          <h4 className="text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
            Item by item
          </h4>
          <ul className="selectable mt-1.5 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface-sunken)] p-2.5 text-xs text-[var(--color-ink-muted)]">
            {/* An item can legitimately repeat — the same file touched twice
                in one batch — so position is part of the key. */}
            {request.items.map((item, index) => (
              <li key={`${item}-${index}`}>{item}</li>
            ))}
          </ul>
          {request.items.length > 1 ? (
            // Said out loud because a list of items invites the assumption
            // that you can pick among them, and you cannot: the answer goes
            // back to the broker whole.
            <p className="mt-1.5 text-xs text-[var(--color-ink-faint)]">
              Your answer applies to all of these.
            </p>
          ) : null}
        </div>
      ) : null}

      {request.paths.length > 0 ? (
        <div className="mt-3">
          <h4 className="text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
            Files involved
          </h4>
          <ul className="selectable mt-1.5 max-h-40 space-y-0.5 overflow-y-auto rounded-md bg-[var(--color-surface-sunken)] p-2.5 font-mono text-xs text-[var(--color-ink-muted)]">
            {request.paths.map((path) => (
              <li key={path} className="truncate" title={path}>
                {path}
              </li>
            ))}
          </ul>
        </div>
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
