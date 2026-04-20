/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";
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

  test("apply_patch restores the checkpoint after a later hunk fails", async () => {
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

      if (command === "restore_checkpoint") {
        await rm(createdFile, { force: true });
        return { success: true };
      }

      throw new Error(`Unexpected command: ${command}`);
    });

    const result = await tools.apply_patch.execute({ patchText }, sdkCtx);

    expect(result).toContain("Created created.ts");
    expect(result).toContain("Failed to create broken.ts: Simulated patch failure");
    expect(result).toContain("Patch failed — restored files to pre-patch state.");
    expect(calls.map((call) => call.command)).toEqual([
      "checkpoint",
      "write",
      "write",
      "restore_checkpoint",
    ]);
    expect(existsSync(createdFile)).toBe(false);
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
    expect(truncatedResult).toBe(
      "1: one\n2: two\n(Showing lines 1-2 of 10. Use startLine/endLine to read other sections.)",
    );
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
});
