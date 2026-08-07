/**
 * Dates, numbers and sizes written the way the reader writes them.
 *
 * 07/08/2026 is the seventh of August in most of Europe and the eighth of
 * July in the United States, and "1,200" is twelve hundred in one place and
 * one point two in another. Guessing is not an option, so every date and
 * every numeral here goes through `Intl` instead of through string
 * concatenation.
 *
 * The locale is a parameter rather than a global. The interface passes
 * nothing, which means the locale the app is running under — the user's own
 * system setting. The tests pass a fixed locale and time zone, so they
 * assert the rule rather than the machine they happen to run on.
 *
 * Words stay in English. The interface's copy is English, and a German
 * reader is better served by "2 minutes" inside an English sentence than by
 * "2 Minuten" wedged into one. What genuinely differs between readers —
 * digit grouping, decimal marks, date order, unit symbols — is what `Intl`
 * decides.
 *
 * Every function is pure, and an unreadable input returns an empty string:
 * callers render nothing rather than "Invalid Date".
 */

/** A parsed timestamp, or undefined when the value is not a date. */
function parseTimestamp(iso: string): Date | undefined {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? undefined : date;
}

/**
 * A timestamp in full: date and time of day, ordered and punctuated for the
 * reader's locale.
 */
export function formatDateTime(
  iso: string,
  locale?: string | undefined,
  timeZone?: string | undefined,
): string {
  const date = parseTimestamp(iso);
  if (!date) return "";
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
    timeZone,
  }).format(date);
}

/**
 * A short timestamp for a list, where a full date on every row would be
 * noise: the time of day for something from today, the day and month within
 * this year, the whole date beyond it. `now` is passed in rather than read
 * from the clock so the choice between the three is testable.
 */
export function formatListTimestamp(
  iso: string,
  now: Date,
  locale?: string | undefined,
  timeZone?: string | undefined,
): string {
  const date = parseTimestamp(iso);
  if (!date) return "";

  // Comparing formatted calendar parts rather than UTC arithmetic: "today"
  // means today in the reader's zone, which is where the day boundary is.
  const dayParts = new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    timeZone,
  });
  if (dayParts.format(date) === dayParts.format(now)) {
    return new Intl.DateTimeFormat(locale, { timeStyle: "short", timeZone }).format(date);
  }

  const yearOf = new Intl.DateTimeFormat(locale, { year: "numeric", timeZone });
  if (yearOf.format(date) === yearOf.format(now)) {
    return new Intl.DateTimeFormat(locale, { day: "numeric", month: "short", timeZone }).format(
      date,
    );
  }

  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeZone }).format(date);
}

/** A whole number with the reader's grouping — 1,200 or 1.200 or 1 200. */
export function formatCount(value: number, locale?: string | undefined): string {
  if (!Number.isFinite(value)) return "";
  return new Intl.NumberFormat(locale).format(value);
}

const SIZE_STEPS = [
  ["byte", 1],
  ["kilobyte", 1e3],
  ["megabyte", 1e6],
  ["gigabyte", 1e9],
  ["terabyte", 1e12],
] as const;

/**
 * A file size in the largest unit that leaves a number a person can read.
 *
 * The steps are decimal, not binary: the symbols `Intl` produces (kB, MB —
 * ko, Mo for a French reader) are the decimal ones, so dividing by 1024 and
 * labelling the result "kB" would be a quiet lie.
 */
export function formatFileSize(bytes: number, locale?: string | undefined): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  let step: (typeof SIZE_STEPS)[number] = SIZE_STEPS[0];
  for (const candidate of SIZE_STEPS) {
    if (bytes >= candidate[1]) step = candidate;
  }
  const [unit, divisor] = step;
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit,
    unitDisplay: "short",
    maximumFractionDigits: unit === "byte" ? 0 : 1,
  }).format(bytes / divisor);
}

const SECOND_MS = 1000;

/**
 * How long something took, at a granularity a person cares about: seconds
 * below a minute, minutes and seconds below an hour, hours and minutes
 * above it.
 */
export function formatDuration(ms: number, locale?: string | undefined): string {
  if (!Number.isFinite(ms) || ms < 0) return "";
  if (ms < SECOND_MS) return "less than a second";

  // Rounded to whole seconds first, so a value that rounds up to a full
  // minute is carried into the minutes rather than shown as "60 seconds".
  const totalSeconds = Math.round(ms / SECOND_MS);
  if (totalSeconds < 60) return countedUnit(totalSeconds, "second", locale);

  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes < 60) {
    const seconds = totalSeconds % 60;
    if (seconds === 0) return countedUnit(totalMinutes, "minute", locale);
    return `${countedUnit(totalMinutes, "minute", locale)} ${countedUnit(seconds, "second", locale)}`;
  }

  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (minutes === 0) return countedUnit(hours, "hour", locale);
  return `${countedUnit(hours, "hour", locale)} ${countedUnit(minutes, "minute", locale)}`;
}

function countedUnit(value: number, unit: string, locale?: string | undefined): string {
  return `${formatCount(value, locale)} ${value === 1 ? unit : `${unit}s`}`;
}
