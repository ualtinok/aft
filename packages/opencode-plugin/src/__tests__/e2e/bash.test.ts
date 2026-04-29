/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, mock, test } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";
import { BridgePool } from "../../pool.js";
import { createBashTool } from "../../tools/bash.js";
import type { PluginContext } from "../../types.js";
import {
  cleanupHarnesses,
  createHarness,
  type E2EHarness,
  type PreparedBinary,
  prepareBinary,
} from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = describe.skipIf(!initialBinary.binaryPath);

interface BashResult {
  /** Agent-visible bash output (what the LLM sees verbatim). */
  output: string;
  /** Last metadata payload pushed via ctx.metadata — exit code, truncation flags, etc. */
  metadata: Record<string, unknown>;
}

interface RuntimeOptions {
  ask?: ToolContext["ask"];
  directory?: string;
  worktree?: string;
}

maybeDescribe("e2e bash command (OpenCode adapter + bridge + Rust)", () => {
  let preparedBinary: PreparedBinary = initialBinary;
  const harnesses: E2EHarness[] = [];

  beforeAll(async () => {
    preparedBinary = await prepareBinary();
  });

  afterEach(async () => {
    await cleanupHarnesses(harnesses);
  });

  async function harness(configOverrides: Record<string, unknown> = {}): Promise<E2EHarness> {
    const created = await createHarness(preparedBinary, {
      fixtureNames: [],
      bridgeOptions: { timeoutMs: 20_000 },
    });
    if (Object.keys(configOverrides).length > 0) {
      await created.bridge.send("configure", {
        project_root: created.tempDir,
        restrict_to_project_root: true,
        bash_permissions: false,
        storage_dir: join(created.tempDir, ".aft-storage"),
        ...configOverrides,
      });
    }
    harnesses.push(created);
    return created;
  }

  async function pluginHarness(configOverrides: Record<string, unknown> = {}) {
    const h = await harness();
    const pool = new BridgePool(
      h.binaryPath,
      { timeoutMs: 20_000 },
      {
        restrict_to_project_root: true,
        bash_permissions: true,
        storage_dir: join(h.tempDir, ".aft-storage"),
        ...configOverrides,
      },
    );
    const ctx: PluginContext = {
      pool,
      client: {} as PluginContext["client"],
      config: {} as PluginContext["config"],
      storageDir: join(h.tempDir, ".aft-storage"),
    };
    const bash = createBashTool(ctx);
    const cleanup = h.cleanup;
    Object.defineProperty(h, "cleanup", {
      value: async () => {
        await pool.shutdown();
        await cleanup.call(h);
      },
    });
    return { h, bash, pool };
  }

  async function callPluginBash(
    bash: ReturnType<typeof createBashTool>,
    h: E2EHarness,
    args: Record<string, unknown>,
    options: RuntimeOptions = {},
  ): Promise<BashResult> {
    let lastMetadata: Record<string, unknown> = {};
    const context = {
      sessionID: "e2e-session",
      messageID: "e2e-message",
      agent: "e2e-agent",
      directory: options.directory ?? h.tempDir,
      worktree: options.worktree ?? h.tempDir,
      abort: new AbortController().signal,
      metadata: (data: Record<string, unknown>) => {
        lastMetadata = data;
      },
      ask: options.ask ?? (async () => {}),
      callID: `call-${Date.now()}`,
    } as ToolContext;
    const output = await bash.execute(args, context);
    return { output: typeof output === "string" ? output : String(output), metadata: lastMetadata };
  }

  test("foreground returns raw output text (not a JSON envelope)", async () => {
    const { h, bash } = await pluginHarness();

    const result = await callPluginBash(bash, h, { command: "echo hello" });

    // Agent-visible output is the raw bash text — NOT a JSON literal that the
    // model would have to JSON.parse before reading.
    expect(result.output).toBe("hello\n");
    // Exit code, truncation, etc. land in metadata for the UI.
    expect(result.metadata.exit).toBe(0);
  });

  test("non-zero exit appends [exit code: N] to agent-visible output", async () => {
    const { h, bash } = await pluginHarness();

    const result = await callPluginBash(bash, h, { command: "false" });

    // The agent must be able to detect command failure from the text itself,
    // because metadata is UI-only and not echoed back to the model.
    expect(result.output).toBe("\n[exit code: 1]");
    expect(result.metadata.exit).toBe(1);
  });

  test("workdir is respected", async () => {
    const { h, bash } = await pluginHarness();
    const subdir = h.path("subdir");
    await mkdir(subdir);

    const result = await callPluginBash(bash, h, { command: "pwd", workdir: subdir });

    expect(result.output.trim()).toBe(await realPath(subdir));
    expect(result.metadata.exit).toBe(0);
  });

  test("foreground timeout returns timed-out process exit without throwing", async () => {
    const h = await harness();

    const response = await h.bridge.send("bash", { command: "sleep 5", timeout: 100 });

    expect(response.success).toBe(true);
    expect(response.timed_out).toBe(true);
    expect(response.exit_code).toBe(124);
  });

  test("rewrites cat to read with footer hint when enabled", async () => {
    const h = await harness({ experimental_bash_rewrite: true });
    const filePath = h.path("notes.txt");
    await writeFile(filePath, "alpha\nbeta\n", "utf8");

    const response = await h.bridge.send("bash", {
      command: `cat ${filePath}`,
      compressed: false,
    });

    expect(response.success).toBe(true);
    expect(String(response.output)).toContain("1: alpha");
    expect(String(response.output)).toContain("call the `read` tool directly next time");
  });

  test("rewrites grep -r to grep tool with footer hint when enabled", async () => {
    const h = await harness({ experimental_bash_rewrite: true });
    await mkdir(h.path("src"));
    await writeFile(h.path("src", "lib.ts"), "needle\nhaystack\n", "utf8");

    const response = await h.bridge.send("bash", {
      command: `grep -r needle ${h.path("src")}`,
      compressed: false,
    });

    expect(response.success).toBe(true);
    expect(String(response.output)).toContain("needle");
    expect(String(response.output)).toContain("call the `grep` tool directly next time");
  });

  test("rewriter disabled runs cat as raw bash without footer", async () => {
    const h = await harness({ experimental_bash_rewrite: false });
    const filePath = h.path("raw.txt");
    await writeFile(filePath, "raw cat output\n", "utf8");

    const response = await h.bridge.send("bash", { command: `cat ${filePath}`, compressed: false });

    expect(response.success).toBe(true);
    expect(response.output).toBe("raw cat output\n");
    expect(String(response.output)).not.toContain("call the `read` tool directly next time");
  });

  test("generic compressor strips ANSI and collapses four-plus duplicate lines", async () => {
    const h = await harness({ experimental_bash_compress: true });

    const response = await h.bridge.send("bash", {
      command: "printf '\\033[31mred\\033[0m\\nred\\nred\\nred\\nred\\n'",
    });

    expect(response.success).toBe(true);
    expect(response.output).toBe("red\n... (4 more)\n");
  });

  test("git status compressor summarizes large status sections", async () => {
    const h = await harness({ experimental_bash_compress: true });
    await h.bridge.send("bash", { command: "git init -q -b main", compressed: false });
    // Status compressor only triggers when output exceeds STATUS_SHORT_LIMIT (1024B);
    // 50 files with longer names easily clears that threshold and exercises the
    // STATUS_KEEP_PER_SECTION (10) truncation path.
    for (let index = 0; index < 50; index++) {
      await writeFile(h.path(`untracked_file_with_long_name_${index}.txt`), `${index}\n`, "utf8");
    }

    const response = await h.bridge.send("bash", { command: "git status" });

    expect(response.success).toBe(true);
    expect(String(response.output)).toContain("Untracked files:");
    // Compressor keeps the first 10 entries per section, then summarizes the rest.
    expect(String(response.output)).toContain("untracked_file_with_long_name_0.txt");
    expect(String(response.output)).toContain("... and 40 more");
    // The summarized entries must NOT appear verbatim — that would mean the
    // compressor didn't actually truncate.
    expect(String(response.output)).not.toContain("untracked_file_with_long_name_49.txt");
  });

  test("compressed false opts out of git status compression", async () => {
    const h = await harness({ experimental_bash_compress: true });
    await h.bridge.send("bash", { command: "git init -q -b main", compressed: false });
    for (let index = 0; index < 15; index++) {
      await writeFile(h.path(`raw_${index}.txt`), `${index}\n`, "utf8");
    }

    const response = await h.bridge.send("bash", { command: "git status", compressed: false });

    expect(response.success).toBe(true);
    expect(String(response.output)).toContain("raw_14.txt");
    expect(String(response.output)).not.toContain("... and 5 more");
  });

  test("background spawn returns task_id immediately", async () => {
    const h = await harness({ experimental_bash_background: true });
    const started = Date.now();

    const response = await h.bridge.send("bash", {
      command: "sleep 1 && echo done",
      background: true,
    });

    expect(response.success).toBe(true);
    expect(response.status).toBe("running");
    expect(typeof response.task_id).toBe("string");
    expect(Date.now() - started).toBeLessThan(750);
  });

  test("bash_status reports running then completed output", async () => {
    const h = await harness({ experimental_bash_background: true });
    const spawned = await h.bridge.send("bash", {
      command: "sleep 0.3 && echo done",
      background: true,
    });
    const taskId = String(spawned.task_id);

    const running = await h.bridge.send("bash_status", { task_id: taskId });
    expect(running.success).toBe(true);
    expect(running.status).toBe("running");

    const completed = await waitForStatus(h, taskId, "completed");
    expect(completed.exit_code).toBe(0);
    expect(completed.output_preview).toBe("done\n");
  });

  test("bash_kill terminates a running task", async () => {
    const h = await harness({ experimental_bash_background: true });
    const spawned = await h.bridge.send("bash", { command: "sleep 60", background: true });
    const taskId = String(spawned.task_id);

    const killed = await h.bridge.send("bash_kill", { task_id: taskId });
    const status = await h.bridge.send("bash_status", { task_id: taskId });

    expect(killed.success).toBe(true);
    expect(killed.status).toBe("killed");
    expect(status.status).toBe("killed");
  });

  test("background completions are no longer appended by the bash adapter", async () => {
    const { h, bash, pool } = await pluginHarness({ experimental_bash_background: true });
    const pluginBridge = pool.getBridge(await realPath(h.tempDir));
    const spawned = await pluginBridge.send("bash", { command: "echo bg-done", background: true });
    const taskId = String(spawned.task_id);
    await new Promise((resolve) => setTimeout(resolve, 1_000));

    const result = await callPluginBash(bash, h, { command: "echo foreground" });

    expect(result.output).toContain("foreground\n");
    expect(result.output).not.toContain("Background tasks completed:");
    expect(result.output).not.toContain(taskId);
    expect(result.output).not.toContain("bg-done");
  });

  test("permission ask round-trip invokes OpenCode ctx.ask", async () => {
    const { h, bash } = await pluginHarness();
    const ask = mock(async () => {});

    const result = await callPluginBash(bash, h, { command: "git status" }, { ask });

    // Real git status fails inside the temp dir (no repo) — exit 128 surfaces
    // in the agent-visible output AND in the metadata.
    expect(result.metadata.exit).toBe(128);
    expect(result.output).toContain("[exit code: 128]");
    expect(ask).toHaveBeenCalledTimes(1);
    expect(ask.mock.calls[0][0]).toMatchObject({
      permission: "bash",
      patterns: ["git status"],
      always: ["git status *"],
    });
  });
});

async function realPath(path: string): Promise<string> {
  const { realpath } = await import("node:fs/promises");
  return realpath(path);
}

async function waitForStatus(
  h: E2EHarness,
  taskId: string,
  expected: string,
): Promise<Record<string, unknown>> {
  const started = Date.now();
  while (Date.now() - started < 5_000) {
    const response = await h.bridge.send("bash_status", { task_id: taskId });
    expect(response.success).toBe(true);
    if (response.status === expected) return response;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for ${expected}`);
}
