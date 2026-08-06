import { describe, expect, it } from "vitest";
import type { Connection } from "@commonspace/protocol";
import { isUsable, recommendedProvider, usableConnections } from "./recommend";

/** A connection with only the fields the recommendation actually reads. */
function connection(provider: string, status: Connection["auth"]["status"]): Connection {
  return {
    provider,
    display_name: provider,
    install: { status: "installed", version: "1.0.0", path: `/usr/local/bin/${provider}` },
    auth: status === "error" ? { status, detail: "" } : { status },
    capabilities: {
      models: ["default"],
      supports_resume: true,
      attachment_types: ["text"],
      supports_permission_bridge: true,
    },
    billing_note: "",
    sign_in_command: "",
    sign_in_explanation: "",
  } as Connection;
}

describe("isUsable", () => {
  it("accepts the three auth kinds a task can run on", () => {
    expect(isUsable(connection("claude_code", "subscription"))).toBe(true);
    expect(isUsable(connection("api_compatible", "api_key"))).toBe(true);
    expect(isUsable(connection("local_model", "local_model"))).toBe(true);
  });

  it("rejects every other auth kind", () => {
    expect(isUsable(connection("claude_code", "signed_out"))).toBe(false);
    expect(isUsable(connection("claude_code", "not_installed"))).toBe(false);
    expect(isUsable(connection("claude_code", "error"))).toBe(false);
  });
});

describe("usableConnections", () => {
  it("keeps the given order", () => {
    const list = [
      connection("gemini_cli", "api_key"),
      connection("claude_code", "signed_out"),
      connection("codex_cli", "subscription"),
    ];
    expect(usableConnections(list).map((c) => c.provider)).toEqual(["gemini_cli", "codex_cli"]);
  });

  it("returns an empty list when nothing is connected", () => {
    expect(usableConnections([])).toEqual([]);
  });
});

describe("recommendedProvider", () => {
  it("has nothing to suggest for an empty list", () => {
    expect(recommendedProvider([])).toBeUndefined();
  });

  it("has nothing to suggest when no connection is usable", () => {
    expect(
      recommendedProvider([
        connection("claude_code", "signed_out"),
        connection("codex_cli", "not_installed"),
        connection("gemini_cli", "error"),
      ]),
    ).toBeUndefined();
  });

  it("suggests the only usable connection", () => {
    expect(
      recommendedProvider([
        connection("claude_code", "signed_out"),
        connection("gemini_cli", "api_key"),
      ]),
    ).toBe("gemini_cli");
  });

  it("prefers a subscription over an API key whatever the list order", () => {
    const subscription = connection("gemini_cli", "subscription");
    const apiKey = connection("claude_code", "api_key");
    expect(recommendedProvider([subscription, apiKey])).toBe("gemini_cli");
    expect(recommendedProvider([apiKey, subscription])).toBe("gemini_cli");
  });

  it("prefers an API key over a local model whatever the list order", () => {
    const apiKey = connection("api_compatible", "api_key");
    const local = connection("local_model", "local_model");
    expect(recommendedProvider([apiKey, local])).toBe("api_compatible");
    expect(recommendedProvider([local, apiKey])).toBe("api_compatible");
  });

  it("breaks a tie by the fixed provider order, not by list position", () => {
    expect(
      recommendedProvider([
        connection("gemini_cli", "subscription"),
        connection("claude_code", "subscription"),
        connection("codex_cli", "subscription"),
      ]),
    ).toBe("claude_code");
  });

  it("sorts a provider outside the fixed order after the ones inside it", () => {
    expect(
      recommendedProvider([
        connection("something_new", "subscription"),
        connection("codex_cli", "subscription"),
      ]),
    ).toBe("codex_cli");
  });

  it("gives the same answer for every ordering of the same connections", () => {
    const list = [
      connection("local_model", "local_model"),
      connection("codex_cli", "api_key"),
      connection("gemini_cli", "subscription"),
      connection("claude_code", "signed_out"),
      connection("api_compatible", "api_key"),
    ];
    const answers = new Set(permutations(list).map((order) => recommendedProvider(order)));
    expect([...answers]).toEqual(["gemini_cli"]);
  });
});

function permutations<T>(items: T[]): T[][] {
  if (items.length <= 1) return [items];
  return items.flatMap((item, index) =>
    permutations([...items.slice(0, index), ...items.slice(index + 1)]).map((rest) => [
      item,
      ...rest,
    ]),
  );
}
