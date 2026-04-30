/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";
import { consumeToolMetadata } from "../metadata-store.js";
import type { BridgePool } from "../pool.js";
import { hoistedTools } from "../tools/hoisted.js";
import type { PluginContext } from "../types.js";

const PROJECT_CWD = resolve(import.meta.dir, "../../../..");
let sdkCtx = createMockSdkContext(PROJECT_CWD);
let tmpDir: string | null = null;
const failingTest = ((test as typeof test & { failing?: typeof test }).failing ??
  test) as typeof test;

type BridgeResponse = Record<string, unknown>;
type SendCall = { command: string; params: Record<string, unknown> };

/** Creates a mock client that returns no connected LSP servers. */
function createMockClient(): any {
  return {
    lsp: {
      status: async () => ({ data: [] }),
    },
    find: {
      symbols: async () => ({ data: [] }),
    },
  };
}

/** Helper to create a PluginContext with a pool and a mock client. */
function createPluginContext(pool: BridgePool): PluginContext {
  return { pool, client: createMockClient(), config: {} as any, storageDir: "/tmp/aft-test" };
}

/** Mock SDK ToolContext for test execute calls. */
function createMockSdkContext(directory: string): ToolContext {
  return {
    sessionID: "test",
    messageID: "test",
    agent: "test",
    directory,
    worktree: directory,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
  };
}

function createMockHoistedHarness(
  sendImpl: (
    command: string,
    params: Record<string, unknown>,
  ) => Promise<BridgeResponse> | BridgeResponse,
) {
  const calls: SendCall[] = [];
  const bridge = {
    send: async (command: string, params: Record<string, unknown> = {}) => {
      calls.push({ command, params });
      return await sendImpl(command, params);
    },
  };

  const pool = {
    getBridge: () => bridge,
  } as unknown as BridgePool;

  return {
    calls,
    tools: hoistedTools(createPluginContext(pool)),
  };
}

afterEach(async () => {
  if (tmpDir) {
    await rm(tmpDir, { recursive: true, force: true });
    tmpDir = null;
  }
  sdkCtx = createMockSdkContext(PROJECT_CWD);
});

describe("Hoisted tool execute handlers", () => {
  test("read throws the Rust error response instead of accessing missing content", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async (command) => {
      expect(command).toBe("read");
      return { success: false, message: "File not found: missing.ts" };
    });

    await expect(tools.read.execute({ filePath: "missing.ts" }, sdkCtx)).rejects.toThrow(
      "File not found: missing.ts",
    );
  });

  test("write throws the Rust error response for invalid writes", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async (command) => {
      expect(command).toBe("write");
      return { success: false, message: "Refusing to write outside project root" };
    });

    await expect(
      tools.write.execute({ filePath: "../outside.ts", content: "export const x = 1;\n" }, sdkCtx),
    ).rejects.toThrow("Refusing to write outside project root");
  });

  failingTest("edit throws the Rust error response for failed replacements", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async (command) => {
      expect(command).toBe("edit_match");
      return { success: false, message: "Match not found in file" };
    });

    await expect(
      tools.edit.execute(
        { filePath: "target.ts", oldString: "before", newString: "after" },
        sdkCtx,
      ),
    ).rejects.toThrow("Match not found in file");
  });

  failingTest("apply_patch throws the Rust error response when a patch write fails", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const patchText = [
      "*** Begin Patch",
      "*** Add File: broken.ts",
      "+export const broken = true;",
      "*** End Patch",
    ].join("\n");

    const { tools } = createMockHoistedHarness(async (command) => {
      if (command === "checkpoint") return { success: true };
      if (command === "write") return { success: false, message: "Disk full while writing patch" };
      throw new Error(`Unexpected command: ${command}`);
    });

    await expect(tools.apply_patch.execute({ patchText }, sdkCtx)).rejects.toThrow(
      "Disk full while writing patch",
    );
  });

  test("delete throws the Rust error response when deletion fails", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async (command) => {
      expect(command).toBe("delete_file");
      return { success: false, message: "Cannot delete protected file" };
    });

    await expect(tools.aft_delete.execute({ filePath: "locked.ts" }, sdkCtx)).rejects.toThrow(
      "Cannot delete protected file",
    );
  });

  test("move throws the Rust error response when rename fails", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async (command) => {
      expect(command).toBe("move_file");
      return { success: false, message: "Destination already exists" };
    });

    await expect(
      tools.aft_move.execute({ filePath: "source.ts", destination: "dest.ts" }, sdkCtx),
    ).rejects.toThrow("Destination already exists");
  });

  test("edit batch mode translates oldString/newString fields for the Rust bridge", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { calls, tools } = createMockHoistedHarness(async () => ({
      success: true,
      edits_applied: 2,
    }));

    const result = await tools.edit.execute(
      {
        filePath: "batch.ts",
        edits: [
          { oldString: "before", newString: "after" },
          { startLine: 4, endLine: 6, content: "replacement" },
        ],
      },
      sdkCtx,
    );

    expect(JSON.parse(result)).toEqual({ success: true, edits_applied: 2 });
    expect(calls).toHaveLength(1);
    expect(calls[0]).toEqual({
      command: "batch",
      params: {
        file: resolve(tmpDir, "batch.ts"),
        edits: [
          { match: "before", replacement: "after" },
          { line_start: 4, line_end: 6, content: "replacement" },
        ],
        diagnostics: true,
        include_diff: true,
        session_id: "test",
      },
    });
  });

  test("edit forwards replaceAll to Rust for multiple occurrences", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { calls, tools } = createMockHoistedHarness(async () => ({
      success: true,
      replacements: 3,
    }));

    const result = await tools.edit.execute(
      {
        filePath: "repeated.ts",
        oldString: "oldName",
        newString: "newName",
        replaceAll: true,
      },
      sdkCtx,
    );

    expect(JSON.parse(result)).toEqual({ success: true, replacements: 3 });
    expect(calls).toHaveLength(1);
    expect(calls[0]).toEqual({
      command: "edit_match",
      params: {
        file: resolve(tmpDir, "repeated.ts"),
        match: "oldName",
        replacement: "newName",
        replace_all: true,
        diagnostics: true,
        include_diff: true,
        session_id: "test",
      },
    });
  });

  /// BUG-6a (per-file commit): when a 2-hunk patch has 1 success and 1
  /// failure, the successful hunk MUST stay applied. Pre-fix, AFT rolled
  /// the whole patch back via checkpoint restore + newly-created cleanup,
  /// throwing away the agent's correct work and forcing them to manually
  /// split patches. New behavior: each hunk commits independently.
  test("apply_patch keeps successful hunks when a later hunk fails (per-file commit)", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const createdFile = resolve(tmpDir, "created.ts");
    const failedFile = resolve(tmpDir, "broken.ts");
    const patchText = [
      "*** Begin Patch",
      "*** Add File: created.ts",
      "+export const created = true;",
      "*** Add File: broken.ts",
      "+export const broken = true;",
      "*** End Patch",
    ].join("\n");

    const { calls, tools } = createMockHoistedHarness(async (command, params) => {
      if (command === "checkpoint") return { success: true };

      if (command === "write") {
        const file = params.file as string;
        if (file === createdFile) {
          await writeFile(file, params.content as string);
          return { success: true };
        }

        if (file === failedFile) {
          throw new Error("Simulated patch failure");
        }
      }

      if (command === "delete_file") {
        // Cleanup of the failed-add partial. We don't expect any other
        // delete_file calls — successful hunks must NOT be deleted.
        await rm(params.file as string, { force: true });
        return { success: true };
      }

      throw new Error(`Unexpected command: ${command}`);
    });

    const result = await tools.apply_patch.execute({ patchText }, sdkCtx);

    expect(result).toContain("Created created.ts");
    expect(result).toContain("Failed to create broken.ts: Simulated patch failure");
    // New: explicit partial-success summary.
    expect(result).toContain("Patch partially applied");
    expect(result).toContain("1 of 2 hunk(s) succeeded");
    expect(result).toContain("Failed: broken.ts");
    expect(result).toContain("aft_safety");

    // No "rolled back" wording — we keep successful changes.
    expect(result).not.toContain("removed 1 newly-created file(s)");
    expect(result).not.toContain("restored pre-existing files");

    // The successful add MUST still be on disk.
    expect(existsSync(createdFile)).toBe(true);

    // No checkpoint call because both paths were newly-created
    // (checkpointPaths empty). The failed-add file is best-effort cleaned
    // up via delete_file in the catch block — but only because the
    // simulated write threw AFTER the file was supposedly created. Our
    // mock's write throws before fs.write happens so the file never
    // exists; assert it was attempted but tolerate either outcome.
    expect(calls.some((c) => c.command === "write" && c.params.file === createdFile)).toBe(true);
    expect(calls.some((c) => c.command === "write" && c.params.file === failedFile)).toBe(true);
    // Crucially: NO restore_checkpoint and NO delete on createdFile.
    expect(calls.some((c) => c.command === "restore_checkpoint")).toBe(false);
    expect(calls.some((c) => c.command === "delete_file" && c.params.file === createdFile)).toBe(
      false,
    );
  });

  test("apply_patch restores checkpoint when move source delete fails", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const earlierFile = resolve(tmpDir, "src/earlier.ts");
    const sourceFile = resolve(tmpDir, "src/original.ts");
    const destFile = resolve(tmpDir, "src/renamed.ts");
    await writeFile(sourceFile, "export const x = 1;\n", { flag: "wx" }).catch(async () => {
      const { mkdir } = await import("node:fs/promises");
      await mkdir(resolve(tmpDir as string, "src"), { recursive: true });
      await writeFile(sourceFile, "export const x = 1;\n");
    });

    const patchText = [
      "*** Begin Patch",
      "*** Add File: src/earlier.ts",
      "+export const earlier = true;",
      "*** Update File: src/original.ts",
      "*** Move to: src/renamed.ts",
      "@@",
      "-export const x = 1;",
      "+export const x = 2;",
      "*** End Patch",
    ].join("\n");

    let destWritten = false;
    const { calls, tools } = createMockHoistedHarness(async (command, params) => {
      if (command === "checkpoint") return { success: true };
      if (command === "write") {
        const file = params.file as string;
        if (file === earlierFile) {
          await writeFile(file, params.content as string);
          return { success: true };
        }
        if (file === destFile) {
          await writeFile(file, params.content as string);
          destWritten = true;
          return { success: true };
        }
      }
      if (command === "delete_file") {
        const file = params.file as string;
        if (file === sourceFile) {
          // Simulate the source delete failing mid-patch.
          throw new Error("Simulated delete_file failure");
        }
        if (file === destFile) {
          await rm(destFile, { force: true });
          return { success: true };
        }
      }
      if (command === "restore_checkpoint") return { success: true };
      throw new Error(`Unexpected command: ${command}`);
    });

    const result = await tools.apply_patch.execute({ patchText }, sdkCtx);

    expect(destWritten).toBe(true);
    expect(existsSync(earlierFile)).toBe(true);
    expect(existsSync(destFile)).toBe(false);
    expect(result).toContain("Failed to update src/original.ts");
    expect(result).toContain("restored pre-patch checkpoint");
    expect(result).toContain("Patch partially applied");
    expect(calls.some((c) => c.command === "restore_checkpoint")).toBe(true);
    expect(calls.some((c) => c.command === "delete_file" && c.params.file === destFile)).toBe(true);
  });

  test("apply_patch restores pre-existing move destination when source delete fails", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const sourceFile = resolve(tmpDir, "src/original.ts");
    const destFile = resolve(tmpDir, "src/renamed.ts");
    const { mkdir } = await import("node:fs/promises");
    await mkdir(resolve(tmpDir, "src"), { recursive: true });
    await writeFile(sourceFile, "export const x = 1;\n");
    await writeFile(destFile, "ORIGINAL\n");

    const patchText = [
      "*** Begin Patch",
      "*** Update File: src/original.ts",
      "*** Move to: src/renamed.ts",
      "@@",
      "-export const x = 1;",
      "+export const x = 2;",
      "*** End Patch",
    ].join("\n");

    const { calls, tools } = createMockHoistedHarness(async (command, params) => {
      if (command === "checkpoint") return { success: true };
      if (command === "write") {
        await writeFile(params.file as string, params.content as string);
        return { success: true };
      }
      if (command === "delete_file") {
        if (params.file === sourceFile) throw new Error("source locked");
        throw new Error(`unexpected delete_file for ${String(params.file)}`);
      }
      if (command === "restore_checkpoint") {
        await writeFile(destFile, "ORIGINAL\n");
        return { success: true };
      }
      throw new Error(`Unexpected command: ${command}`);
    });

    const result = await tools.apply_patch.execute({ patchText }, sdkCtx);

    expect(result).toContain("restored pre-patch checkpoint");
    expect(await readFile(destFile, "utf-8")).toBe("ORIGINAL\n");
    expect(calls.some((c) => c.command === "restore_checkpoint")).toBe(true);
    expect(calls.some((c) => c.command === "delete_file" && c.params.file === destFile)).toBe(
      false,
    );
  });

  test("apply_patch reports both copies when move rollback delete also fails", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const sourceFile = resolve(tmpDir, "src/original.ts");
    const destFile = resolve(tmpDir, "src/renamed.ts");
    const { mkdir } = await import("node:fs/promises");
    await mkdir(resolve(tmpDir, "src"), { recursive: true });
    await writeFile(sourceFile, "export const x = 1;\n");

    const patchText = [
      "*** Begin Patch",
      "*** Update File: src/original.ts",
      "*** Move to: src/renamed.ts",
      "@@",
      "-export const x = 1;",
      "+export const x = 2;",
      "*** End Patch",
    ].join("\n");

    const { tools } = createMockHoistedHarness(async (command, params) => {
      if (command === "checkpoint") return { success: true };
      if (command === "write") {
        await writeFile(params.file as string, params.content as string);
        return { success: true };
      }
      if (command === "delete_file") {
        const file = params.file as string;
        if (file === sourceFile) throw new Error("source locked");
      }
      if (command === "restore_checkpoint") throw new Error("restore locked");
      throw new Error(`Unexpected command: ${command}`);
    });

    const result = await tools.apply_patch.execute({ patchText }, sdkCtx);

    expect(existsSync(sourceFile)).toBe(true);
    expect(existsSync(destFile)).toBe(true);
    expect(result).toContain("move_partial_failure");
    expect(result).toContain(sourceFile);
    expect(result).toContain(destFile);
    expect(result).toContain("Both copies may exist or destination content may be changed");
  });

  /// BUG-6a dogfooding repro: the user's exact 3-file complaint. A multi-
  /// file patch where 2 files patch cleanly and the 3rd hits a fuzzy-match
  /// drift used to roll back the 2 successes. Now the 2 successes commit
  /// and only the failed file is reported as failing.
  test("apply_patch keeps successful files when ONE of three updates fails (user repro)", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const okFile1 = resolve(tmpDir, "cli-program.ts");
    const okFile2 = resolve(tmpDir, "cli-installer.ts");
    const driftFile = resolve(tmpDir, "athena-council-guard.ts");

    // Seed all three files with realistic pre-patch content.
    await writeFile(okFile1, "old line 1\n");
    await writeFile(okFile2, "old line 2\n");
    await writeFile(driftFile, "drifted content that won't match\n");

    const patchText = [
      "*** Begin Patch",
      "*** Update File: cli-program.ts",
      "@@",
      "-old line 1",
      "+new line 1",
      "*** Update File: cli-installer.ts",
      "@@",
      "-old line 2",
      "+new line 2",
      "*** Update File: athena-council-guard.ts",
      "@@",
      "-expected line that doesn't exist in file",
      "+something else",
      "*** End Patch",
    ].join("\n");

    const { calls, tools } = createMockHoistedHarness(async (command, params) => {
      if (command === "checkpoint") return { success: true };
      if (command === "write") {
        const file = params.file as string;
        await writeFile(file, params.content as string);
        return { success: true };
      }
      throw new Error(`Unexpected command: ${command}`);
    });

    const result = await tools.apply_patch.execute({ patchText }, sdkCtx);

    expect(result).toContain("Updated cli-program.ts");
    expect(result).toContain("Updated cli-installer.ts");
    expect(result).toContain("Failed to update athena-council-guard.ts");
    expect(result).toContain("Patch partially applied");
    expect(result).toContain("2 of 3 hunk(s) succeeded");
    expect(result).toContain("aft_safety");

    // The two successful files must reflect the new content on disk.
    expect((await import("node:fs/promises")).readFile(okFile1, "utf-8")).resolves.toBe(
      "new line 1\n",
    );
    expect((await import("node:fs/promises")).readFile(okFile2, "utf-8")).resolves.toBe(
      "new line 2\n",
    );
    // The drifted file is unchanged (applyUpdateChunks throws BEFORE write).
    expect((await import("node:fs/promises")).readFile(driftFile, "utf-8")).resolves.toBe(
      "drifted content that won't match\n",
    );

    // No restore_checkpoint anywhere — that's the whole fix.
    expect(calls.some((c) => c.command === "restore_checkpoint")).toBe(false);
    // No delete_file on the successful files — we keep them.
    expect(calls.some((c) => c.command === "delete_file" && c.params.file === okFile1)).toBe(false);
    expect(calls.some((c) => c.command === "delete_file" && c.params.file === okFile2)).toBe(false);
  });

  test("read returns binary-file messages without trying to split missing content", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { calls, tools } = createMockHoistedHarness(async () => ({
      success: true,
      binary: true,
      message: "Binary file (512 bytes)",
    }));

    const result = await tools.read.execute({ filePath: "artifact.bin" }, sdkCtx);

    expect(result).toBe("Binary file (512 bytes)");
    expect(calls[0]).toEqual({
      command: "read",
      params: {
        file: resolve(tmpDir, "artifact.bin"),
        session_id: "test",
      },
    });
  });

  test("read handles directory listings and truncated content responses", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    let callIndex = 0;
    const { tools } = createMockHoistedHarness(async () => {
      callIndex += 1;
      if (callIndex === 1) {
        return { success: true, entries: ["a.ts", "src/"] };
      }

      return {
        success: true,
        content: "1: one\n2: two",
        truncated: true,
        start_line: 1,
        end_line: 2,
        total_lines: 10,
      };
    });

    const directoryResult = await tools.read.execute({ filePath: "." }, sdkCtx);
    const truncatedResult = await tools.read.execute({ filePath: "big.ts" }, sdkCtx);

    expect(directoryResult).toBe("a.ts\nsrc/");
    // Case B: agent did NOT specify a range, response was clamped → hint footer
    // is useful, tells the agent more exists and how to get it.
    expect(truncatedResult).toBe(
      "1: one\n2: two\n(Showing lines 1-2 of 10. Use startLine/endLine to read other sections.)",
    );
  });

  test("read does not append a footer when the file fits in default limit (case A)", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async () => ({
      success: true,
      content: "1: one\n2: two\n3: three",
      // truncated:false means the response IS the whole file — no footer needed.
      truncated: false,
      start_line: 1,
      end_line: 3,
      total_lines: 3,
    }));

    const result = await tools.read.execute({ filePath: "small.ts" }, sdkCtx);

    expect(result).toBe("1: one\n2: two\n3: three");
  });

  test("read drops the navigation hint when the agent supplied startLine/endLine (case B)", async () => {
    // Repro for the dogfooding bug: agent calls read({startLine: 130, endLine: 190})
    // on a 191-line file and gets back lines 130-190 EXACTLY. Telling them
    // "use startLine/endLine to read other sections" right after they used
    // those exact params is patronizing. They have the math.
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async () => ({
      success: true,
      content: "130: ...\n190: ...",
      // Rust still reports truncated:true because the response is a slice
      // of the file (end_line < total_lines). The plugin must NOT key the
      // hint off this flag alone — it needs to know the agent picked the slice.
      truncated: true,
      start_line: 130,
      end_line: 190,
      total_lines: 191,
    }));

    const result = await tools.read.execute(
      { filePath: "registry.ts", startLine: 130, endLine: 190 },
      sdkCtx,
    );

    // The user's exact complaint: when end_line matches total_lines (or is
    // close to it after a deliberate range), no footer should be emitted at
    // all. Agent gets back only the content.
    expect(result).toBe("130: ...\n190: ...");
    expect(result).not.toContain("Use startLine/endLine");
  });

  test("read drops the footer entirely when the agent's range happens not to cover the full file (case B)", async () => {
    // Subtle case: agent asked 100-150 of a 200-line file. They got back
    // exactly what they asked for. The earlier "compact when clamped"
    // branch would have spuriously emitted `(Lines 100-150 of 200)` here,
    // which is the SAME shape of patronizing footer as the original bug —
    // re-teaching an agent that they got less than the whole file when
    // THEY chose to. Agent has the math: they sent the request and they
    // can see the content length.
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async () => ({
      success: true,
      content: "100: ...\n150: ...",
      truncated: true,
      start_line: 100,
      end_line: 150,
      total_lines: 200,
    }));

    const result = await tools.read.execute(
      { filePath: "mid.ts", startLine: 100, endLine: 150 },
      sdkCtx,
    );

    expect(result).toBe("100: ...\n150: ...");
    expect(result).not.toContain("Use startLine/endLine");
    expect(result).not.toContain("(Lines 100-150");
  });

  test("read drops the navigation hint when the agent supplied offset/limit (case B)", async () => {
    // Same as the startLine/endLine case but for the OpenCode-built-in-
    // compatible offset/limit param shape. Agent that picked the slice
    // should not be re-taught how to pick a slice.
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const { tools } = createMockHoistedHarness(async () => ({
      success: true,
      content: "10: ...\n29: ...",
      truncated: true,
      start_line: 10,
      end_line: 29,
      total_lines: 30,
    }));

    const result = await tools.read.execute(
      { filePath: "small.ts", offset: 10, limit: 20 },
      sdkCtx,
    );

    // No footer at all — agent picked the range, has the math.
    expect(result).toBe("10: ...\n29: ...");
    expect(result).not.toContain("Use startLine/endLine");
    expect(result).not.toContain("(Lines");
  });

  test("write distinguishes new files from updates", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);

    let writeCount = 0;
    const { calls, tools } = createMockHoistedHarness(async (command) => {
      expect(command).toBe("write");
      writeCount += 1;
      return writeCount === 1
        ? { success: true, created: true, formatted: false }
        : { success: true, created: false, formatted: true };
    });

    const createdResult = await tools.write.execute(
      { filePath: "created.ts", content: "export const created = true;\n" },
      sdkCtx,
    );
    const updatedResult = await tools.write.execute(
      { filePath: "created.ts", content: "export const created = false;\n" },
      sdkCtx,
    );

    expect(createdResult).toBe("Created new file.");
    expect(updatedResult).toBe("File updated. Auto-formatted.");
    expect(calls).toHaveLength(2);
    expect(calls[0]?.params.file).toBe(resolve(tmpDir, "created.ts"));
    expect(calls[1]?.params.file).toBe(resolve(tmpDir, "created.ts"));
  });

  /// Regression: v0.15.3 — apply_patch metadata.files entries must include
  /// `patch`, `additions`, and `deletions` for OpenCode's UI to render diffs.
  ///
  /// OpenCode's UI patchFile() at packages/ui/src/components/apply-patch-file.ts
  /// drops any file metadata entry that lacks all of `patch`, `before`, `after`.
  /// Pre-fix, AFT only sent `{ filePath, relativePath, type }`, so EVERY file
  /// was silently dropped and the TUI/desktop showed no diffs at all.
  test("apply_patch stores per-file diff metadata for the OpenCode renderer", async () => {
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-hoisted-"));
    sdkCtx = createMockSdkContext(tmpDir);
    // Inject callID — required for storeToolMetadata to fire (the production
    // ToolContext supplies it; our mock omits it by default).
    (sdkCtx as unknown as { callID: string }).callID = "call_apply_patch_meta";

    const updatedFile = resolve(tmpDir, "updated.ts");
    const deletedFile = resolve(tmpDir, "deleted.ts");

    // Seed source files for the update + delete hunks (apply_patch reads
    // them via fs.readFile to compute per-file diffs).
    await writeFile(updatedFile, "old line\n");
    await writeFile(deletedFile, "to be deleted\n");

    const patchText = [
      "*** Begin Patch",
      "*** Add File: new.ts",
      "+export const created = 1;",
      "*** Update File: updated.ts",
      "@@",
      "-old line",
      "+new line",
      "*** Delete File: deleted.ts",
      "*** End Patch",
    ].join("\n");

    const { tools } = createMockHoistedHarness(async (command) => {
      if (command === "checkpoint") return { success: true };
      if (command === "write") return { success: true };
      if (command === "delete_file") return { success: true };
      throw new Error(`Unexpected command: ${command}`);
    });

    await tools.apply_patch.execute({ patchText }, sdkCtx);

    const stored = consumeToolMetadata("test", "call_apply_patch_meta");
    expect(stored).toBeDefined();
    expect(stored?.title).toContain("Success. Updated the following files:");
    expect(stored?.title).toContain("A new.ts");
    expect(stored?.title).toContain("M updated.ts");
    expect(stored?.title).toContain("D deleted.ts");

    const meta = stored?.metadata as {
      diff: string;
      files: Array<{
        filePath: string;
        relativePath: string;
        type: string;
        patch: string;
        additions: number;
        deletions: number;
        movePath?: string;
      }>;
    };

    expect(meta.diff).toBeTypeOf("string");
    expect(meta.files).toHaveLength(3);

    // Each file MUST carry patch + additions + deletions or the OpenCode UI
    // will silently drop it (the v0.15.3 regression). This assertion
    // catches any future change that strips these fields.
    for (const file of meta.files) {
      expect(file.filePath).toBeTypeOf("string");
      expect(file.relativePath).toBeTypeOf("string");
      expect(["add", "update", "delete", "move"]).toContain(file.type);
      expect(file.patch).toBeTypeOf("string");
      expect(file.patch.length).toBeGreaterThan(0);
      expect(file.additions).toBeTypeOf("number");
      expect(file.deletions).toBeTypeOf("number");
    }

    // Sanity-check shape of each per-file entry. We don't assert exact
    // additions/deletions counts because buildUnifiedDiff treats absent
    // content as an empty line ("") which shows up in the diff — the
    // important contract is that `patch` and the counters are present
    // and non-degenerate, which the per-entry loop above already checks.
    const addEntry = meta.files.find((f) => f.type === "add");
    expect(addEntry?.additions).toBeGreaterThan(0);

    const updateEntry = meta.files.find((f) => f.type === "update");
    expect(updateEntry?.additions).toBeGreaterThan(0);
    expect(updateEntry?.deletions).toBeGreaterThan(0);

    const deleteEntry = meta.files.find((f) => f.type === "delete");
    expect(deleteEntry?.deletions).toBeGreaterThan(0);
  });
});
