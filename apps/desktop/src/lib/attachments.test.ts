import { describe, expect, it } from "vitest";
import { mergePaths } from "./attachments";

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
