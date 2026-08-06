import type { JSX, ReactNode } from "react";
import { Button, Card } from "./primitives";

export interface OnboardingProps {
  hasProject: boolean;
  hasConnection: boolean;
  /** Opens the Projects screen. */
  onChooseFolder: () => void;
  /** Opens the Connections screen. */
  onConnectAgent: () => void;
}

interface Step {
  title: string;
  body: ReactNode;
  done: boolean;
  /** Absent on the last step, which is guidance rather than a task. */
  action?: { label: string; onClick: () => void } | undefined;
}

/**
 * The first-run path: the two things Commonspace cannot do without, then
 * what to say once it can. Shown until both are settled — the caller stops
 * rendering it at that point, so there is no congratulations state here.
 */
export function Onboarding({
  hasProject,
  hasConnection,
  onChooseFolder,
  onConnectAgent,
}: OnboardingProps): JSX.Element {
  const steps: Step[] = [
    {
      title: "Choose a folder to work in",
      body: "Commonspace only sees the folders you pick. Start with the one holding the files this job is about — you can add more later, or take them away.",
      done: hasProject,
      action: { label: "Choose a folder", onClick: onChooseFolder },
    },
    {
      title: "Connect an agent you already pay for",
      body: "Commonspace runs on the official tools from Anthropic, OpenAI and others. You sign in once inside their tool, and Commonspace uses that sign-in. There is no account here, and we charge nothing on top of what you already pay them.",
      done: hasConnection,
      action: { label: "Open Connections", onClick: onConnectAgent },
    },
    {
      title: "Describe the first job",
      body: "Ask for something concrete: summarize these contracts, turn this folder of PDFs into a spreadsheet, rename these scans by date. Commonspace asks before it creates, changes, or deletes anything.",
      done: false,
    },
  ];

  // The one thing to do next. Later steps stay reachable, but only this one
  // gets a prominent button, so there is nothing to work out.
  const nextIndex = steps.findIndex((step) => !step.done && step.action);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-2xl px-6 py-8">
        <h1 className="text-lg font-semibold">Setting up</h1>
        <p className="mt-1 text-sm text-[var(--color-ink-muted)]">
          Three steps, once. Nothing runs and no files change until you ask for something.
        </p>

        <ol className="mt-6 space-y-3">
          {steps.map((step, index) => (
            <Card as="li" key={step.title} className="p-4">
              <div className="flex items-start gap-3">
                <StepMarker number={index + 1} total={steps.length} done={step.done} />
                <div className="min-w-0 flex-1">
                  <h2 className="text-sm font-semibold">{step.title}</h2>
                  <p className="mt-1 text-sm text-[var(--color-ink-muted)]">{step.body}</p>
                  {step.action && !step.done ? (
                    <Button
                      className="mt-3"
                      variant={index === nextIndex ? "primary" : "secondary"}
                      onClick={step.action.onClick}
                    >
                      {step.action.label}
                    </Button>
                  ) : null}
                </div>
              </div>
            </Card>
          ))}
        </ol>
      </div>
    </div>
  );
}

/**
 * The step's number, or a tick once it is settled. The tick is decorative:
 * the status also travels as text for screen readers, so it never rests on
 * colour or a glyph alone (WCAG 1.4.1).
 */
function StepMarker({
  number,
  total,
  done,
}: {
  number: number;
  total: number;
  done: boolean;
}) {
  return (
    <span
      className={`mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold ${
        done
          ? "bg-[var(--color-ok-soft)] text-[var(--color-ok)]"
          : "bg-[var(--color-surface-sunken)] text-[var(--color-ink-muted)]"
      }`}
    >
      <span aria-hidden="true">{done ? "✓" : number}</span>
      <span className="sr-only">
        {done ? `Step ${number} of ${total}, done.` : `Step ${number} of ${total}, not done yet.`}
      </span>
    </span>
  );
}
