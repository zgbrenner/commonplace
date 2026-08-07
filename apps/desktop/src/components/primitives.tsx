/**
 * The small set of shared primitives Commonspace's interface is built from.
 *
 * Written directly rather than pulled from a component library: the surface
 * is small, and owning it keeps the visual language consistent and the
 * bundle light. Every interactive element is keyboard reachable, labelled,
 * and shows a visible focus ring (the global `:focus-visible` rule).
 */
import type { ButtonHTMLAttributes, ReactNode } from "react";

type ButtonVariant = "primary" | "secondary" | "quiet" | "danger";

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: "sm" | "md";
}

const buttonVariants: Record<ButtonVariant, string> = {
  primary:
    "bg-[var(--color-accent)] text-white hover:bg-[var(--color-accent-hover)] disabled:bg-[var(--color-line-strong)]",
  secondary:
    "bg-[var(--color-surface-raised)] text-[var(--color-ink)] border border-[var(--color-line-strong)] hover:bg-[var(--color-surface-sunken)]",
  quiet:
    "bg-transparent text-[var(--color-ink-muted)] hover:bg-[var(--color-surface-sunken)] hover:text-[var(--color-ink)]",
  danger:
    "bg-[var(--color-danger)] text-white hover:brightness-110 disabled:bg-[var(--color-line-strong)]",
};

export function Button({
  variant = "secondary",
  size = "md",
  className = "",
  ...props
}: ButtonProps) {
  const sizing = size === "sm" ? "px-2.5 py-1 text-[0.8125rem]" : "px-3.5 py-1.5 text-sm";
  return (
    <button
      type="button"
      {...props}
      className={`inline-flex items-center justify-center gap-1.5 rounded-md font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-60 ${sizing} ${buttonVariants[variant]} ${className}`}
    />
  );
}

/**
 * A status pill. Status is never carried by colour alone: every pill has a
 * text label, and callers pass a glyph for a second non-colour channel
 * (WCAG 1.4.1).
 */
export function StatusPill({
  tone,
  glyph,
  children,
}: {
  tone: "ok" | "warn" | "danger" | "neutral" | "accent";
  glyph?: string;
  children: ReactNode;
}) {
  const tones = {
    ok: "bg-[var(--color-ok-soft)] text-[var(--color-ok)]",
    warn: "bg-[var(--color-warn-soft)] text-[var(--color-warn)]",
    danger: "bg-[var(--color-danger-soft)] text-[var(--color-danger)]",
    accent: "bg-[var(--color-accent-soft)] text-[var(--color-accent)]",
    neutral: "bg-[var(--color-surface-sunken)] text-[var(--color-ink-muted)]",
  } as const;
  return (
    // The `status-pill` class carries no styling of its own; it is the hook
    // styles.css needs to draw a boundary under Windows High Contrast, where
    // the OS replaces the tone background with its own.
    <span
      className={`status-pill inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium ${tones[tone]}`}
    >
      {glyph ? <span aria-hidden="true">{glyph}</span> : null}
      {children}
    </span>
  );
}

export function Card({
  children,
  className = "",
  as: Tag = "div",
}: {
  children: ReactNode;
  className?: string;
  as?: "div" | "section" | "article" | "li";
}) {
  return (
    // `card` is the same kind of hook as `status-pill`: no styling here, a
    // forced-colors boundary in styles.css.
    <Tag
      className={`card rounded-[var(--radius-card)] border border-[var(--color-line)] bg-[var(--color-surface-raised)] ${className}`}
    >
      {children}
    </Tag>
  );
}

/** A calm empty state: what this area is for, and the one next action. */
export function EmptyState({
  title,
  description,
  action,
}: {
  title: string;
  description: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center px-8 text-center">
      <h2 className="text-base font-semibold text-[var(--color-ink)]">{title}</h2>
      <p className="mt-1.5 max-w-md text-sm text-[var(--color-ink-muted)]">{description}</p>
      {action ? <div className="mt-4">{action}</div> : null}
    </div>
  );
}

/**
 * The collapsible technical view. Closed by default everywhere — the whole
 * product promise is that this is optional.
 */
export function TechnicalDetails({
  children,
  label = "Technical details",
}: {
  children: ReactNode;
  label?: string;
}) {
  return (
    <details className="group mt-2">
      <summary className="cursor-pointer list-none text-xs text-[var(--color-ink-faint)] hover:text-[var(--color-ink-muted)]">
        <span aria-hidden="true" className="mr-1 inline-block group-open:rotate-90">
          ›
        </span>
        {label}
      </summary>
      <div className="selectable mt-2 overflow-x-auto rounded-md bg-[var(--color-surface-sunken)] p-3 font-mono text-xs whitespace-pre-wrap text-[var(--color-ink-muted)]">
        {children}
      </div>
    </details>
  );
}

/**
 * A closed-by-default disclosure whose body is ordinary content, not raw
 * text. Same summary affordance as `TechnicalDetails`, so the two read as
 * one idea: the optional layer, folded away until asked for.
 */
export function Disclosure({ label, children }: { label: string; children: ReactNode }) {
  return (
    <details className="group mt-2">
      <summary className="cursor-pointer list-none text-xs text-[var(--color-ink-faint)] hover:text-[var(--color-ink-muted)]">
        <span aria-hidden="true" className="mr-1 inline-block group-open:rotate-90">
          ›
        </span>
        {label}
      </summary>
      <div className="mt-2">{children}</div>
    </details>
  );
}

/** Inline error with a recovery action, never a bare stack trace. */
export function ErrorNotice({
  message,
  recovery,
  onRetry,
  announce = true,
}: {
  message: string;
  recovery?: string | undefined;
  onRetry?: (() => void) | undefined;
  /**
   * Whether this notice speaks for itself. True everywhere an error arrives
   * out of nowhere and interrupting is the right thing. The conversation
   * column passes false: it has one live region of its own, which already
   * says the task didn't finish, and two voices reading the same failure is
   * worse than one.
   */
  announce?: boolean;
}) {
  return (
    <div
      role={announce ? "alert" : undefined}
      className="rounded-[var(--radius-card)] border border-[var(--color-danger)] bg-[var(--color-danger-soft)] p-3"
    >
      <p className="text-sm font-medium text-[var(--color-danger)]">
        <span aria-hidden="true" className="mr-1.5">
          ⚠
        </span>
        <span className="sr-only">Error: </span>
        {message}
      </p>
      {recovery ? (
        <p className="mt-1 text-sm text-[var(--color-ink-muted)]">{recovery}</p>
      ) : null}
      {onRetry ? (
        <Button size="sm" className="mt-2" onClick={onRetry}>
          Try again
        </Button>
      ) : null}
    </div>
  );
}

/** Section heading used across panels. */
export function PanelHeading({ children }: { children: ReactNode }) {
  return (
    <h2 className="px-4 pt-4 pb-2 text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
      {children}
    </h2>
  );
}
