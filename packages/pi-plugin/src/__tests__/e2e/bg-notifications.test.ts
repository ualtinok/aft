/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import {
  __resetBgNotificationStateForTests,
  appendToolResultBgCompletions,
  handleTurnEndBgCompletions,
  trackBgTask,
} from "../../bg-notifications.js";
import { registerBashTool } from "../../tools/bash.js";
import type { PluginContext } from "../../types.js";
import {
  createHarness,
  type Harness,
  type MockExtensionContext,
  type MockToolDef,
  prepareBinary,
} from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = initialBinary.binaryPath ? describe : describe.skip;

maybeDescribe("e2e bg notifications (Pi adapter + bridge + Rust)", () => {
  let harnesses: Harness[] = [];

  beforeAll(async () => {
    await prepareBinary();
  });

  afterEach(async () => {
    __resetBgNotificationStateForTests();
    await Promise.allSettled(harnesses.map((harness) => harness.cleanup()));
    harnesses = [];
  });

  async function pluginHarness() {
    const h = await createHarness(initialBinary, {
      fixtureNames: [],
      config: { search_index: false, experimental_bash_background: true } as never,
      timeoutMs: 60_000,
    });
    harnesses.push(h);
    const tools = new Map<string, MockToolDef>();
    registerBashTool({ registerTool: (tool: MockToolDef) => tools.set(tool.name, tool) } as never, {
      pool: h.pool,
      config: {} as PluginContext["config"],
      storageDir: h.path(".aft-storage"),
    });
    return { h, bash: tools.get("bash")! };
  }

  test("tool_result delivery appends reminder after another tool result", async () => {
    const { h, bash } = await pluginHarness();
    const taskId = await spawnBackground(h, bash, "printf done");

    let content: Array<{ type: "text"; text: string }> | undefined;
    await waitUntil(async () => {
      content = (await appendToolResultBgCompletions(
        {
          ctx: {
            pool: h.pool,
            config: {} as PluginContext["config"],
            storageDir: h.path(".aft-storage"),
          },
          directory: h.tempDir,
          sessionID: undefined,
        },
        [{ type: "text", text: "tool output" }],
      )) as Array<{ type: "text"; text: string }> | undefined;
      return Boolean(
        content?.some((block) => block.type === "text" && block.text.includes(taskId)),
      );
    });

    const reminder = content?.at(-1)?.text ?? "";
    expect(reminder).toContain("<system-reminder>");
    expect(reminder).toContain(`- task ${taskId} (exit 0): printf done`);
  });

  test("turn-end wake sends runtime user message", async () => {
    const { h, bash } = await pluginHarness();
    const taskId = await spawnBackground(h, bash, "printf idle-done");
    const messages: string[] = [];

    await waitUntil(async () => {
      await handleTurnEndBgCompletions({
        ctx: {
          pool: h.pool,
          config: {} as PluginContext["config"],
          storageDir: h.path(".aft-storage"),
        },
        directory: h.tempDir,
        sessionID: undefined,
        runtime: { sendUserMessage: (message: string) => messages.push(message) },
      });
      await sleep(260);
      return messages.length > 0;
    });

    expect(messages).toHaveLength(1);
    expect(messages[0]).toContain(`- task ${taskId} (exit 0): printf idle-done`);
  });
});

async function spawnBackground(h: Harness, bash: MockToolDef, command: string): Promise<string> {
  const extCtx: MockExtensionContext = { cwd: h.tempDir, hasUI: false };
  const result = await bash.execute(
    `test-bash-${Date.now()}`,
    { command, background: true },
    undefined,
    undefined,
    extCtx,
  );
  const taskId = String(
    result.details && typeof result.details === "object"
      ? (result.details as { task_id?: string }).task_id
      : "",
  );
  trackBgTask(undefined, taskId);
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
