/**
 * E2E coverage for aft_outline + aft_zoom.
 */

/// <reference path="../../bun-test.d.ts" />

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { createHarness, type Harness, prepareBinary } from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = initialBinary.binaryPath ? describe : describe.skip;

maybeDescribe("aft_outline + aft_zoom (real bridge)", () => {
  let harness: Harness;

  beforeAll(async () => {
    harness = await createHarness(initialBinary);
  });

  afterAll(async () => {
    if (harness) await harness.cleanup();
  });

  test("outline single file — sample.ts lists functions and class", async () => {
    const result = await harness.callTool("aft_outline", { target: "sample.ts" });
    const text = harness.text(result);
    expect(text).toContain("funcA");
    expect(text).toContain("funcB");
    expect(text).toContain("SampleService");
  });

  test("outline single file keeps text details shape", async () => {
    await writeFile(harness.path("single.ts"), "export function single() { return 1; }\n", "utf8");

    const result = await harness.callTool("aft_outline", { target: "single.ts" });
    const text = harness.text(result);

    expect(result.details).toBeUndefined();
    expect(text).toContain("single.ts");
    expect(text).toContain("single");
  });

  test("outline batched files via array target", async () => {
    const result = await harness.callTool("aft_outline", {
      target: [harness.path("sample.ts"), harness.path("imports.ts")],
    });
    const text = harness.text(result);
    expect(text).toContain("sample.ts");
    expect(text).toContain("imports.ts");
  });

  test("outline array target keeps text details shape", async () => {
    await writeFile(harness.path("array-a.ts"), "export function arrayA() { return 1; }\n", "utf8");
    await writeFile(harness.path("array-b.ts"), "export function arrayB() { return 2; }\n", "utf8");

    const result = await harness.callTool("aft_outline", {
      target: [harness.path("array-a.ts"), harness.path("array-b.ts")],
    });
    const text = harness.text(result);

    expect(result.details).toBeUndefined();
    expect(text).toContain("array-a.ts");
    expect(text).toContain("array-b.ts");
    expect(text).toContain("arrayA");
    expect(text).toContain("arrayB");
  });

  test("outline directory via target", async () => {
    const result = await harness.callTool("aft_outline", { target: "." });
    const text = harness.text(result);
    expect(text).toContain("sample.ts");
    // Go file should be included
    expect(text).toContain("sample.go");
  });

  test("outline rejects empty string target", async () => {
    await expect(harness.callTool("aft_outline", { target: "" })).rejects.toThrow(/non-empty/);
  });

  test("outline auto-detects directory passed as string target", async () => {
    const result = await harness.callTool("aft_outline", { target: "directory" });
    const text = harness.text(result);
    // Directory mode returned (tree output) — real content depends on fixture
    expect(text.length).toBeGreaterThan(0);
  });

  test("outline directory target returns complete true below walk cap", async () => {
    await mkdir(harness.path("outline-small"), { recursive: true });
    await writeFile(
      harness.path("outline-small", "good.ts"),
      "export function good() { return 1; }\n",
      "utf8",
    );
    await writeFile(harness.path("outline-small", "bad.ts"), "export function bad( {\n", "utf8");

    const result = await harness.callTool("aft_outline", { target: "outline-small" });
    const response = result.details as Record<string, unknown>;

    expect(response.complete).toBe(true);
    expect(response.walk_truncated).toBe(false);
    const skipped = response.skipped_files as Array<{ file: string; reason: string }>;
    expect(skipped).toHaveLength(1);
    expect(skipped[0].file).toMatch(/outline-small[/\\]bad\.ts$/);
    expect(skipped[0].reason).toBe("parse_error");
    expect(harness.text(result)).toContain("good.ts");
    expect(harness.text(result)).toContain("good");
  });

  test("outline directory target returns complete false when Rust walk truncates", async () => {
    await mkdir(harness.path("outline-large"), { recursive: true });
    for (let index = 0; index < 205; index += 1) {
      await writeFile(
        harness.path("outline-large", `file-${String(index).padStart(3, "0")}.ts`),
        `export const value${index} = ${index};\n`,
        "utf8",
      );
    }

    const result = await harness.callTool("aft_outline", { target: "outline-large" });
    const response = result.details as Record<string, unknown>;

    expect(response.complete).toBe(false);
    expect(response.walk_truncated).toBe(true);
    expect(Array.isArray(response.skipped_files)).toBe(true);
    expect(harness.text(result)).toContain("file-000.ts");
  });

  test("zoom into single symbol returns source", async () => {
    const result = await harness.callTool("aft_zoom", {
      filePath: "sample.ts",
      symbol: "funcB",
    });
    const text = harness.text(result);
    expect(text).toContain("funcB");
    expect(text).toContain("normalize");
  });

  test("zoom multi-symbol returns array", async () => {
    const result = await harness.callTool("aft_zoom", {
      filePath: "sample.ts",
      symbols: ["funcA", "funcB"],
    });
    const text = harness.text(result);
    // Array-shaped JSON: two results
    expect(text).toContain("funcA");
    expect(text).toContain("funcB");
  });

  test("zoom with contextLines expands range", async () => {
    const result = await harness.callTool("aft_zoom", {
      filePath: "sample.ts",
      symbol: "funcA",
      contextLines: 10,
    });
    const text = harness.text(result);
    expect(text).toContain("funcA");
  });
});
