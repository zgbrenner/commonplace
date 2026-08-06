import { describe, expect, it } from "vitest";
import { createLatestGuard } from "./search";

describe("createLatestGuard", () => {
  it("recognizes the request it was told about", () => {
    const guard = createLatestGuard<string>();
    guard.begin("apples");
    expect(guard.isCurrent("apples")).toBe(true);
  });

  it("treats nothing as current before any request begins", () => {
    const guard = createLatestGuard<string>();
    expect(guard.isCurrent("apples")).toBe(false);
    // Even undefined — the unset sentinel — must not read as current.
    expect(createLatestGuard<string | undefined>().isCurrent(undefined)).toBe(false);
  });

  it("invalidates earlier requests when a newer one begins", () => {
    const guard = createLatestGuard<string>();
    guard.begin("apples");
    guard.begin("apples and pears");
    expect(guard.isCurrent("apples")).toBe(false);
    expect(guard.isCurrent("apples and pears")).toBe(true);
  });

  it("ignores an out-of-order response for a superseded query", () => {
    // The scenario the guard exists for: request A goes out, request B goes
    // out, then A's response arrives last. Only B's response may apply.
    const guard = createLatestGuard<string>();
    const applied: string[] = [];
    const send = (query: string) => {
      guard.begin(query);
      return (response: string) => {
        if (guard.isCurrent(query)) applied.push(response);
      };
    };
    const resolveA = send("a");
    const resolveB = send("ab");
    resolveB("results for ab");
    resolveA("results for a"); // late arrival — must be dropped
    expect(applied).toEqual(["results for ab"]);
  });

  it("treats re-issuing the same key as current again", () => {
    const guard = createLatestGuard<string>();
    guard.begin("apples");
    guard.begin("pears");
    guard.begin("apples");
    expect(guard.isCurrent("apples")).toBe(true);
    expect(guard.isCurrent("pears")).toBe(false);
  });
});
