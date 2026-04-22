/**
 * Unit tests for the Pi-compatible line-numbered diff formatter used by
 * hoisted write/edit renderers.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import { formatDiffForPi } from "../tools/diff-format.js";

describe("formatDiffForPi", () => {
  test("returns empty diff when contents are identical", () => {
    const result = formatDiffForPi("line\n", "line\n");
    expect(result.diff).toBe("");
    expect(result.firstChangedLine).toBeUndefined();
  });

  test("formats a single-line change with Pi's +NN / -NN shape", () => {
    const before = "const a = 1;\n";
    const after = "const a = 2;\n";
    const { diff, firstChangedLine } = formatDiffForPi(before, after);

    // Pi expects one-indexed line numbers.
    expect(firstChangedLine).toBe(1);
    // At least one removed and one added line with the expected prefix.
    expect(diff).toMatch(/^-\s*1 const a = 1;$/m);
    expect(diff).toMatch(/^\+\s*1 const a = 2;$/m);
  });

  test("emits context lines with leading space and line number", () => {
    const before = "a\nb\nc\nd\ne\n";
    const after = "a\nb\nC\nd\ne\n";
    const { diff, firstChangedLine } = formatDiffForPi(before, after);

    expect(firstChangedLine).toBe(3);
    // Context lines: one leading space + (possibly padded) line number + " content".
    expect(diff).toMatch(/^ 1 a$/m);
    expect(diff).toMatch(/^ 2 b$/m);
    expect(diff).toMatch(/^-3 c$/m);
    expect(diff).toMatch(/^\+3 C$/m);
    expect(diff).toMatch(/^ 4 d$/m);
  });

  test("collapses large unchanged ranges with '...' marker", () => {
    // 20 identical lines surrounded by one change at start and end — forces
    // the middle to collapse.
    const lines = Array.from({ length: 20 }, (_, i) => `line${i}`).join("\n");
    const before = `A\n${lines}\nEND\n`;
    const after = `B\n${lines}\nEND\n`;

    const { diff } = formatDiffForPi(before, after, 2);
    expect(diff).toContain("...");
  });

  test("pads line numbers to uniform width for mixed single/double-digit output", () => {
    // 12 lines → width 2. Change line 10 (content "L9") so we see both
    // single-digit padded context and double-digit no-pad numbers in one run.
    const before = `${Array.from({ length: 12 }, (_, i) => `L${i}`).join("\n")}\n`;
    const after = before.replace("L9", "CHANGED");

    const { diff, firstChangedLine } = formatDiffForPi(before, after, 2);
    expect(firstChangedLine).toBe(10);

    // Exact expected output: 9 leading unchanged lines collapse behind "...",
    // then two context lines (L7 at line 8, L8 at line 9) with width-2 padding,
    // then the change at line 10, then trailing context.
    expect(diff).toBe(
      [
        "    ...", // " " + width-2 blank + " " + "..." = 4 spaces + "..."
        "  8 L7", // leading space + padded " 8" + " " + content
        "  9 L8",
        "-10 L9", // no leading space on change lines
        "+10 CHANGED",
        " 11 L10", // trailing context — width-2 "11" needs no padding
        " 12 L11",
      ].join("\n"),
    );
  });

  test("contextLines=0 emits only the collapse marker (Pi parity)", () => {
    // Bug guard: slice(-0) === slice(0) returns the full array, so a naive
    // implementation would dump every leading context line when contextLines
    // is zero. Pi's reference implementation emits only "...".
    const before = `${Array.from({ length: 6 }, (_, i) => `L${i}`).join("\n")}\n`;
    const after = before.replace("L5", "CHANGED");

    const { diff } = formatDiffForPi(before, after, 0);

    expect(diff).toBe(["   ...", "-6 L5", "+6 CHANGED"].join("\n"));
    expect(diff).not.toContain("L0");
    expect(diff).not.toContain("L4");
  });
});
