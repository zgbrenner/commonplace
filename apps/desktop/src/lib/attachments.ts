/**
 * Attachment list helpers.
 *
 * Pure so they can be unit tested without a DOM. Both the file picker and
 * native drag-and-drop feed through `mergePaths`, so the two ways of
 * attaching behave identically.
 */

/**
 * Merge newly chosen paths into the current attachment list, keeping the
 * existing order and dropping duplicates (including duplicates within the
 * new batch). Always returns a fresh array, so it is safe to hand straight
 * to a state setter.
 */
export function mergePaths(current: readonly string[], added: readonly string[]): string[] {
  return [...new Set([...current, ...added])];
}

/**
 * The one-line disclosure shown above the composer when attachments are
 * present: what Commonspace will look at, and where what it reads may go.
 *
 * We say "attached item(s)" rather than "file(s) and folder(s)": the frontend
 * only holds path strings and does no filesystem access, and a path string
 * alone cannot reliably tell a file from a folder — honesty over precision.
 * The backend, which does stat the paths, records the real kind.
 *
 * Deliberately no token or size estimates here; the roadmap defers those to
 * a future Details-level view.
 */
export function attachmentDisclosure(count: number, providerName: string | undefined): string {
  const items = count === 1 ? "1 attached item" : `${count} attached items`;
  const destination =
    providerName && providerName.trim().length > 0 ? providerName : "your connected agent";
  return `Commonspace will look at ${items}. What it reads may be sent to ${destination}. Your files won't be changed without your approval.`;
}
