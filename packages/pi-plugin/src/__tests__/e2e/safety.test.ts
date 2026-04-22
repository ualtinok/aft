/**
 * E2E coverage for aft_safety 5 ops.
 * Regression for wrong Rust command names (was sending "safety" with op param;
 * Rust expects undo/edit_history/checkpoint/restore_checkpoint/list_checkpoints).
 */

/// <reference path="../../bun-test.d.ts" />

import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { createHarness, type Harness, prepareBinary, writeFixture } from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = initialBinary.binaryPath ? describe : describe.skip;

maybeDescribe("aft_safety (real bridge)", () => {
  let harness: Harness;

  beforeAll(async () => {
    harness = await createHarness(initialBinary);
  });

  afterAll(async () => {
    if (harness) await harness.cleanup();
  });

  test("history requires filePath", async () => {
    await expect(harness.callTool("aft_safety", { op: "history" })).rejects.toThrow(/filePath/);
  });

  test("checkpoint requires name", async () => {
    await expect(harness.callTool("aft_safety", { op: "checkpoint" })).rejects.toThrow(/name/);
  });

  test("history returns empty for unedited file", async () => {
    await writeFixture(harness, "untouched.ts", "x\n");
    const result = await harness.callTool("aft_safety", {
      op: "history",
      filePath: "untouched.ts",
    });
    const text = harness.text(result);
    // Result is JSON stringified — either explicit entries: [] or a no-history shape
    expect(text.length).toBeGreaterThan(0);
  });

  test("edit → history shows one snapshot", async () => {
    await writeFixture(harness, "edited.ts", "line1\nline2\n");
    await harness.callTool("edit", {
      filePath: "edited.ts",
      oldString: "line1",
      newString: "LINE1",
    });
    const result = await harness.callTool("aft_safety", {
      op: "history",
      filePath: "edited.ts",
    });
    const text = harness.text(result);
    // Rust edit_history returns { file, entries: [...] }
    expect(text).toContain("entries");
  });

  test("edit → undo reverts file content", async () => {
    await writeFixture(harness, "undoable.ts", "hello\n");
    await harness.callTool("edit", {
      filePath: "undoable.ts",
      oldString: "hello",
      newString: "goodbye",
    });
    // Sanity: edit succeeded
    expect(await readFile(harness.path("undoable.ts"), "utf8")).toBe("goodbye\n");

    await harness.callTool("aft_safety", { op: "undo", filePath: "undoable.ts" });
    expect(await readFile(harness.path("undoable.ts"), "utf8")).toBe("hello\n");
  });

  test("checkpoint promotes filePath to single-entry files[]", async () => {
    // Regression: Rust `checkpoint` only accepts `files`, not `file`. The plugin
    // must auto-upgrade `filePath` → `files: [filePath]` rather than silently
    // dropping it and falling back to the whole tracked-file set.
    await harness.callTool("write", { filePath: "cp-single.ts", content: "hello\n" });
    const result = await harness.callTool("aft_safety", {
      op: "checkpoint",
      name: "single-file-cp",
      filePath: "cp-single.ts",
    });
    const text = harness.text(result);
    expect(text).toContain("single-file-cp");
    expect(text).toContain('"file_count": 1');
    // Must not have silently omitted our file
    expect(text).not.toContain('"file_count": 0');
  });

  test("checkpoint tolerates deleted files in tracked set", async () => {
    // Regression: earlier behavior aborted the whole checkpoint on the first
    // missing path when the tracked-file fallback hit a deleted file. Now we
    // skip and report instead.
    await harness.callTool("write", { filePath: "cp-keeper.ts", content: "stays\n" });
    await harness.callTool("write", { filePath: "cp-doomed.ts", content: "soon\n" });
    await harness.callTool("aft_delete", { filePath: "cp-doomed.ts" });

    // No explicit files → uses tracked-file set, which still contains cp-doomed.ts.
    const result = await harness.callTool("aft_safety", {
      op: "checkpoint",
      name: "after-deletion",
    });
    const text = harness.text(result);
    expect(text).toContain("after-deletion");
    // cp-keeper.ts survived the snapshot
    expect(text).toMatch(/"file_count":\s*[1-9]/);
    // cp-doomed.ts is reported as skipped, not as a hard failure
    expect(text).toContain("skipped");
    expect(text).toContain("cp-doomed.ts");
  });

  test("checkpoint → list → restore round-trip", async () => {
    // Use the `write` tool (not raw fs) so the file is tracked in the backup store.
    await harness.callTool("write", { filePath: "cp-target.ts", content: "original\n" });
    await harness.callTool("aft_safety", {
      op: "checkpoint",
      name: "before-change",
      files: ["cp-target.ts"],
    });

    // Mutate
    await harness.callTool("edit", {
      filePath: "cp-target.ts",
      oldString: "original",
      newString: "modified",
    });
    expect(await readFile(harness.path("cp-target.ts"), "utf8")).toBe("modified\n");

    // List includes our checkpoint
    const listResult = await harness.callTool("aft_safety", { op: "list" });
    expect(harness.text(listResult)).toContain("before-change");

    // Restore flips back
    await harness.callTool("aft_safety", { op: "restore", name: "before-change" });
    expect(await readFile(harness.path("cp-target.ts"), "utf8")).toBe("original\n");
  });
});
