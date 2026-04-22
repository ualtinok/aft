/**
 * Unit tests for `buildMutationResult` — the bridge-response-to-Pi-tool-result
 * shaper used by hoisted write/edit. Exercises truncation, diagnostics, and
 * no-op paths without spinning up a real bridge.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { buildMutationResult } from "../tools/hoisted.js";

describe("buildMutationResult", () => {
  test("surfaces truncation in both text and details", () => {
    const result = buildMutationResult("src/big.ts", {
      replacements: 1,
      diff: {
        additions: 42,
        deletions: 17,
        truncated: true,
        // before/after omitted because Rust skips them on truncated diffs.
      },
    });

    // Agent-facing text includes explicit truncation notice.
    const text = result.content
      .filter((c) => c.type === "text")
      .map((c) => (c as { text?: string }).text ?? "")
      .join("");
    expect(text).toContain("Edited src/big.ts (+42/-17, 1 replacement)");
    expect(text).toContain("diff truncated");
    expect(text).not.toContain("\n+"); // no actual diff lines leaked

    // Details expose the truncation flag so the TUI renderer can surface it.
    expect(result.details?.truncated).toBe(true);
    expect(result.details?.diff).toBeUndefined();
    expect(result.details?.firstChangedLine).toBeUndefined();
    expect(result.details?.additions).toBe(42);
    expect(result.details?.deletions).toBe(17);
  });

  test("produces a real Pi-style diff when before/after are present", () => {
    const result = buildMutationResult("src/small.ts", {
      replacements: 1,
      diff: {
        additions: 1,
        deletions: 1,
        truncated: false,
        before: "const a = 1;\n",
        after: "const a = 2;\n",
      },
    });

    expect(result.details?.truncated).toBeUndefined();
    expect(result.details?.firstChangedLine).toBe(1);
    expect(result.details?.diff).toMatch(/^-\s*1 const a = 1;$/m);
    expect(result.details?.diff).toMatch(/^\+\s*1 const a = 2;$/m);

    const text = result.content
      .filter((c) => c.type === "text")
      .map((c) => (c as { text?: string }).text ?? "")
      .join("");
    expect(text).toContain("Edited src/small.ts (+1/-1, 1 replacement)");
    expect(text).toContain("-1 const a = 1;");
    expect(text).toContain("+1 const a = 2;");
    expect(text).not.toContain("diff truncated");
  });

  test("write path (no replacements) produces the 'Wrote …' header", () => {
    const result = buildMutationResult("src/new.ts", {
      diff: {
        additions: 10,
        deletions: 0,
        truncated: false,
        before: "",
        after: "line1\nline2\nline3\n",
      },
    });

    const text = result.content
      .filter((c) => c.type === "text")
      .map((c) => (c as { text?: string }).text ?? "")
      .join("");
    expect(text).toMatch(/^Wrote src\/new\.ts \(\+10\/-0\)/);
    expect(result.details?.replacements).toBeUndefined();
  });

  test("appends LSP diagnostics in a human-readable block", () => {
    const result = buildMutationResult("src/bad.ts", {
      replacements: 1,
      diff: {
        additions: 1,
        deletions: 1,
        truncated: false,
        before: "const x: number = 1;\n",
        after: "const x: string = 1;\n",
      },
      lsp_diagnostics: [
        {
          line: 1,
          severity: "error",
          message: "Type 'number' is not assignable to type 'string'.",
        },
      ],
    });

    const text = result.content
      .filter((c) => c.type === "text")
      .map((c) => (c as { text?: string }).text ?? "")
      .join("");
    expect(text).toContain("LSP diagnostics:");
    expect(text).toContain("[error] line 1: Type 'number'");
  });

  test("no-op edit returns zero counts without a diff block", () => {
    const result = buildMutationResult("src/unchanged.ts", {
      replacements: 0,
      diff: {
        additions: 0,
        deletions: 0,
        truncated: false,
        before: "const a = 1;\n",
        after: "const a = 1;\n",
      },
    });

    expect(result.details?.diff).toBe("");
    expect(result.details?.additions).toBe(0);
    expect(result.details?.deletions).toBe(0);
    expect(result.details?.truncated).toBeUndefined();
  });
});
