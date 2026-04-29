/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import type { ToolContext } from "@opencode-ai/plugin";
import {
  __resetBgNotificationStateForTests,
  appendInTurnBgCompletions,
  handleIdleBgCompletions,
  trackBgTask,
} from "../../bg-notifications.js";
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

maybeDescribe("e2e bg notifications (OpenCode adapter + bridge + Rust)", () => {
  let preparedBinary: PreparedBinary = initialBinary;
  const harnesses: E2EHarness[] = [];

  beforeAll(async () => {
    preparedBinary = await prepareBinary();
  });

  afterEach(async () => {
    __resetBgNotificationStateForTests();
    await cleanupHarnesses(harnesses);
  });

  async function pluginHarness() {
    const h = await createHarness(preparedBinary, {
      fixtureNames: [],
      bridgeOptions: { timeoutMs: 20_000 },
    });
    harnesses.push(h);
    const pool = new BridgePool(
      h.binaryPath,
      { timeoutMs: 20_000 },
      {
        project_root: h.tempDir,
        restrict_to_project_root: false,
        bash_permissions: false,
        experimental_bash_background: true,
        storage_dir: h.path(".aft-storage"),
      },
    );
    const ctx: PluginContext = {
      pool,
      client: {} as PluginContext["client"],
      config: {} as PluginContext["config"],
      storageDir: h.path(".aft-storage"),
    };
    const cleanup = h.cleanup;
    Object.defineProperty(h, "cleanup", {
      value: async () => {
        await pool.shutdown();
        await cleanup.call(h);
      },
    });
    return { h, ctx, bash: createBashTool(ctx) };
  }

  test("in-turn delivery appends reminder after another tool result", async () => {
    const { h, ctx, bash } = await pluginHarness();
    const taskId = await spawnBackground(h, bash, "printf done");
    const output = { output: "read output", title: "read", metadata: {} };

    await waitUntil(async () => {
      await appendInTurnBgCompletions(
        { ctx, directory: h.tempDir, sessionID: "e2e-session" },
        output,
      );
      return output.output.includes(taskId);
    });

    expect(output.output).toContain("<system-reminder>");
    expect(output.output).toContain(`- task ${taskId} (exit 0): printf done`);
  });

  test("turn-end wake sends promptAsync through OpenCode client", async () => {
    const { h, ctx, bash } = await pluginHarness();
    const taskId = await spawnBackground(h, bash, "printf idle-done");
    const promptCalls: unknown[] = [];
    const client = {
      session: { promptAsync: async (payload: unknown) => promptCalls.push(payload) },
    };

    await waitUntil(async () => {
      await handleIdleBgCompletions({
        ctx,
        directory: h.tempDir,
        sessionID: "e2e-session",
        client,
      });
      await sleep(260);
      return promptCalls.length > 0;
    });

    expect(promptCalls).toHaveLength(1);
    const text = (promptCalls[0] as { body: { parts: Array<{ text: string }> } }).body.parts[0]
      .text;
    expect(text).toContain(`- task ${taskId} (exit 0): printf idle-done`);
  });
});

async function spawnBackground(
  h: E2EHarness,
  bash: ReturnType<typeof createBashTool>,
  command: string,
): Promise<string> {
  const output = await bash.execute({ command, background: true }, {
    sessionID: "e2e-session",
    messageID: "e2e-message",
    agent: "e2e-agent",
    directory: h.tempDir,
    worktree: h.tempDir,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
    callID: `call-${Date.now()}`,
  } as ToolContext);
  const taskId = String(output).replace("Background task started: ", "");
  trackBgTask("e2e-session", taskId);
  return taskId;
}

async function waitUntil(predicate: () => Promise<boolean>, timeoutMs = 4_000): Promise<void> {
  const started = Date.now();
  while (!(await predicate())) {
    if (Date.now() - started > timeoutMs) throw new Error("timed out waiting for condition");
    await sleep(100);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
