/**
 * Contract tests for the schemas defined locally in `ipc.ts` (as opposed to
 * the shared `@commonspace/protocol` shapes, which `protocol.test.ts`
 * validates against Rust-generated fixtures).
 *
 * The fixtures here mirror what the Rust `search_history` command returns:
 * plain-text snippets with … ellipses, never HTML.
 */
import { describe, expect, it } from "vitest";
import { z } from "zod";
import { searchResultSchema } from "./ipc";

const searchResponseFixture = [
  {
    kind: "conversation",
    conversation_id: "conv-01J0000000000000000000TEST",
    title: "Rename the 2024 scans by date",
    snippet: "Rename the 2024 scans by date",
    created_at: "2026-08-01T09:15:00Z",
  },
  {
    kind: "message",
    conversation_id: "conv-01J0000000000000000000TEST",
    title: "Rename the 2024 scans by date",
    snippet: "…found 37 files matching scan_*.pdf and renamed them to their scan dates…",
    created_at: "2026-08-01T09:16:42Z",
  },
  {
    kind: "message",
    conversation_id: "conv-01J0000000000000000001OTHR",
    title: "Untitled task",
    snippet: "…the spreadsheet now has one row per invoice…",
    created_at: "2026-07-30T17:03:10Z",
  },
];

describe("searchResultSchema", () => {
  it("accepts a full search_history response, exactly as ipc.searchHistory parses it", () => {
    const parsed = z.array(searchResultSchema).parse(searchResponseFixture);
    expect(parsed).toHaveLength(3);
    expect(parsed[0]?.kind).toBe("conversation");
    expect(parsed[1]?.kind).toBe("message");
    // The snippet round-trips untouched, ellipses included — it is plain
    // text the sidebar renders directly.
    expect(parsed[1]?.snippet).toBe(
      "…found 37 files matching scan_*.pdf and renamed them to their scan dates…",
    );
  });

  it("rejects a kind outside the contract", () => {
    const bad = { ...searchResponseFixture[0], kind: "workspace" };
    expect(searchResultSchema.safeParse(bad).success).toBe(false);
  });

  it("rejects a result missing its conversation id", () => {
    const bad: Record<string, unknown> = { ...searchResponseFixture[0] };
    delete bad["conversation_id"];
    expect(searchResultSchema.safeParse(bad).success).toBe(false);
  });

  it("rejects non-string snippets rather than coercing them", () => {
    const bad = { ...searchResponseFixture[0], snippet: 42 };
    expect(searchResultSchema.safeParse(bad).success).toBe(false);
  });
});
