/**
 * Which connected agent Commonspace suggests, and which connections it can
 * run a task with at all.
 *
 * Pure so it can be unit tested without a DOM, and shared so the composer and
 * the app shell always agree on what "connected" means.
 */
import type { Connection } from "@commonspace/protocol";

/**
 * A connection Commonspace can actually run a task with today: the agent is
 * authenticated in one of the three ways the backend knows how to drive.
 * Every other auth status (not installed, signed out, error) means there is
 * nothing to run.
 */
export function isUsable(connection: Connection): boolean {
  const status = connection.auth.status;
  return status === "subscription" || status === "api_key" || status === "local_model";
}

/** The connections a task can be sent to, in the order they were given. */
export function usableConnections(connections: Connection[]): Connection[] {
  return connections.filter(isUsable);
}

/**
 * How the three usable auth kinds rank against each other. Lower sorts first.
 *
 * The reasoning, which is a product judgement rather than a technical fact:
 * a subscription is already paid for, so running a task on it costs the person
 * nothing extra. An API key bills per token, so every task quietly adds to a
 * bill — fine when chosen, wrong as a default. A local model costs nothing and
 * sends nothing away, but is today noticeably slower and weaker at the
 * long-running, file-editing work Commonspace asks for, so suggesting it first
 * would set someone up to be disappointed. Anyone who prefers local can still
 * pick it; this only decides what is suggested when they have not said.
 */
const authRank: Record<string, number> = {
  subscription: 0,
  api_key: 1,
  local_model: 2,
};

/**
 * The order used to break a tie between connections of equal auth kind. It
 * follows the provider list in the protocol, so the suggestion does not drift
 * when connections happen to be listed in a different order. A provider not
 * named here sorts after the ones that are, and then by input order.
 */
const providerOrder: readonly string[] = [
  "claude_code",
  "codex_cli",
  "gemini_cli",
  "open_code",
  "api_compatible",
  "local_model",
];

/**
 * The agent Commonspace suggests when the person has not chosen one.
 * Undefined when nothing is usable.
 *
 * Deterministic: the same set of connections gives the same answer whatever
 * order they arrive in.
 */
export function recommendedProvider(connections: Connection[]): string | undefined {
  const usable = usableConnections(connections);
  if (usable.length === 0) return undefined;

  let best = usable[0];
  if (!best) return undefined;
  for (const candidate of usable.slice(1)) {
    if (compare(candidate, best) < 0) best = candidate;
  }
  return best.provider;
}

/** Negative when `a` is the better suggestion. Input order is the last word. */
function compare(a: Connection, b: Connection): number {
  const byAuth = (authRank[a.auth.status] ?? 99) - (authRank[b.auth.status] ?? 99);
  if (byAuth !== 0) return byAuth;
  return providerIndex(a.provider) - providerIndex(b.provider);
}

function providerIndex(provider: string): number {
  const index = providerOrder.indexOf(provider);
  return index === -1 ? providerOrder.length : index;
}
