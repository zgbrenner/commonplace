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
