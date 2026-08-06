import { describe, expect, it } from "vitest";
import { attachmentDisclosure, mergePaths } from "./attachments";

describe("mergePaths", () => {
  it("appends new paths after the existing ones, in order", () => {
    expect(mergePaths(["/a/one.txt"], ["/b/two.txt", "/c/three.txt"])).toEqual([
      "/a/one.txt",
      "/b/two.txt",
      "/c/three.txt",
    ]);
  });

  it("drops paths that are already attached", () => {
    expect(mergePaths(["/a/one.txt", "/b/two.txt"], ["/b/two.txt"])).toEqual([
      "/a/one.txt",
      "/b/two.txt",
    ]);
  });

  it("drops duplicates within the newly dropped batch", () => {
    expect(mergePaths([], ["/a/one.txt", "/a/one.txt"])).toEqual(["/a/one.txt"]);
  });

  it("treats paths as case-sensitive strings, exactly like the picker", () => {
    // A deliberate decision, matching the previous Set-based picker code:
    // path normalization belongs to the backend, not to the UI.
    expect(mergePaths(["/a/One.txt"], ["/a/one.txt"])).toHaveLength(2);
  });

  it("returns a fresh array and never mutates its inputs", () => {
    const current = ["/a/one.txt"];
    const added = ["/b/two.txt"];
    const merged = mergePaths(current, added);
    expect(merged).not.toBe(current);
    expect(current).toEqual(["/a/one.txt"]);
    expect(added).toEqual(["/b/two.txt"]);
  });

  it("handles both sides being empty", () => {
    expect(mergePaths([], [])).toEqual([]);
  });
});

describe("attachmentDisclosure", () => {
  it("uses the singular for one item", () => {
    expect(attachmentDisclosure(1, "Claude Code")).toBe(
      "Commonspace will look at 1 attached item. What it reads may be sent to Claude Code. Your files won't be changed without your approval.",
    );
  });

  it("uses the plural for several items", () => {
    expect(attachmentDisclosure(3, "Claude Code")).toBe(
      "Commonspace will look at 3 attached items. What it reads may be sent to Claude Code. Your files won't be changed without your approval.",
    );
  });

  it("says 'attached items', never guessing files versus folders", () => {
    // The frontend only has path strings, so it never claims to know which
    // paths are files and which are folders.
    const copy = attachmentDisclosure(2, "Claude Code");
    expect(copy).toContain("attached items");
    expect(copy).not.toMatch(/\bfiles?\b and\b/);
    expect(copy).not.toContain("folder");
  });

  it("falls back to a generic destination when no provider name is known", () => {
    expect(attachmentDisclosure(2, undefined)).toContain(
      "may be sent to your connected agent",
    );
    expect(attachmentDisclosure(2, "  ")).toContain(
      "may be sent to your connected agent",
    );
  });

  it("never includes token or size estimates", () => {
    // Deferred by the roadmap to a future Details-level view.
    expect(attachmentDisclosure(5, "Claude Code")).not.toMatch(/token|byte|KB|MB/i);
  });
});
