/**
 * The other half of the IPC contract check.
 *
 * `crates/commonspace-core/tests/protocol_fixtures.rs` serializes one
 * representative value of every type that crosses the Tauri boundary into
 * `tests/fixtures/protocol-samples.json`. This test validates that exact file
 * against the Zod schemas the UI actually uses.
 *
 * If a Rust type changes without `packages/protocol` following, one of the two
 * tests fails — so the drift cannot be merged quietly.
 */
// Imported as JSON rather than read with `node:fs`: the app's tsconfig
// deliberately carries no Node types, because nothing in this application
// runs outside the webview.
import fixture from "../../../../tests/fixtures/protocol-samples.json";
import { describe, expect, it } from "vitest";
import {
  adapterCapabilitiesSchema,
  agentEventSchema,
  artifactKindSchema,
  artifactSchema,
  authStatusSchema,
  healthReportSchema,
  installStatusSchema,
  operationClassSchema,
  operationResultSchema,
  permissionRequestSchema,
  providerIdSchema,
  taskPlanSchema,
  taskStateSchema,
} from "@commonspace/protocol";
import type { z } from "zod";

const samples = fixture as unknown as Record<string, unknown>;

/** Parse and surface Zod's message directly, so a failure names the field. */
function expectValid(schema: z.ZodTypeAny, value: unknown, label: string) {
  const result = schema.safeParse(value);
  if (!result.success) {
    throw new Error(`${label} did not match the schema:\n${result.error.message}`);
  }
}

describe("IPC contract parity with the Rust types", () => {
  it("accepts every normalized event Rust can emit", () => {
    const events = samples["events"] as unknown[];
    expect(events.length).toBeGreaterThan(10);
    for (const event of events) {
      const tag = (event as { type: string }).type;
      expectValid(agentEventSchema, event, `event ${tag}`);
    }
  });

  it("covers every event variant the schema declares", () => {
    // Guards against a Rust variant being added to the union without a
    // fixture, which would let it go unvalidated here.
    const emitted = new Set(
      (samples["events"] as { type: string }[]).map((event) => event.type),
    );
    const declared = agentEventSchema.options.map(
      (option) => option.shape.type.value as string,
    );
    const missing = declared.filter((name) => !emitted.has(name));
    expect(missing, `event variants missing from the fixture: ${missing.join(", ")}`).toEqual(
      [],
    );
  });

  it.each([
    ["install_status", installStatusSchema],
    ["auth_status", authStatusSchema],
    ["provider_ids", providerIdSchema],
    ["task_states", taskStateSchema],
    ["operation_classes", operationClassSchema],
    ["artifact_kinds", artifactKindSchema],
    ["operation_results", operationResultSchema],
  ])("accepts every %s value", (key, schema) => {
    const values = samples[key] as unknown[];
    expect(Array.isArray(values)).toBe(true);
    values.forEach((value, index) => expectValid(schema, value, `${key}[${index}]`));
  });

  it.each([
    ["capabilities", adapterCapabilitiesSchema],
    ["health_report", healthReportSchema],
    ["permission_request", permissionRequestSchema],
    ["plan", taskPlanSchema],
    ["artifact", artifactSchema],
  ])("accepts the %s shape", (key, schema) => {
    expectValid(schema, samples[key], key);
  });

  it("rejects a payload the backend could never send", () => {
    // Sanity check that the schemas are actually discriminating, rather than
    // accepting anything and making the tests above vacuous.
    expect(agentEventSchema.safeParse({ type: "not.a.real.event" }).success).toBe(false);
    expect(authStatusSchema.safeParse({ status: "definitely_not_a_status" }).success).toBe(
      false,
    );
    expect(artifactKindSchema.safeParse("spreadsheet").success).toBe(false);
  });
});
