import type { TaskSuggestion } from "../lib/ipc";
import { Button } from "./primitives";

/**
 * Concrete first jobs, drawn from what is actually in the project's folders.
 *
 * Renders nothing when the backend found nothing worth offering: a generic
 * suggestion describes Commonspace rather than the person's files, which is
 * worse than an empty space.
 *
 * Picking one sends it straight away rather than typing it into the composer.
 * That is safe because nothing runs until the plan is approved — the next
 * thing on screen is a plan with Start, Change plan and Cancel.
 */
export function Suggestions({
  suggestions,
  onPick,
  disabled,
}: {
  suggestions: TaskSuggestion[];
  onPick: (prompt: string) => void;
  disabled: boolean;
}) {
  if (suggestions.length === 0) return null;

  return (
    <section className="mx-auto w-full max-w-3xl px-6 pb-3">
      <h2 className="text-xs font-semibold tracking-wide text-[var(--color-ink-faint)] uppercase">
        From your files
      </h2>
      <ul className="mt-2 flex flex-wrap gap-2">
        {suggestions.map((suggestion) => (
          <li key={suggestion.id}>
            <Button
              size="sm"
              onClick={() => onPick(suggestion.prompt)}
              disabled={disabled}
              // The full request on hover, so picking one is not a leap of
              // faith about what the short label means.
              title={suggestion.prompt}
            >
              {suggestion.label}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}
