/// <reference path="../bun-test.d.ts" />
import { describe, expect, mock, test } from "bun:test";
import { resolve } from "node:path";
import type { BridgePool, BridgeRequestOptions } from "@cortexkit/aft-bridge";
import { type ToolContext, tool } from "@opencode-ai/plugin";
import { consumeToolMetadata } from "../metadata-store.js";
import { createBashKillTool, createBashStatusTool, createBashTool } from "../tools/bash.js";
import type { PluginContext } from "../types.js";

const PROJECT_CWD = resolve(import.meta.dir, "../../../..");

type BridgeResponse = Record<string, unknown>;
type SendCall = {
  command: string;
  params: Record<string, unknown>;
  options?: BridgeRequestOptions;
};
type ProgressHandler = (frame: { text: string }) => void;
type SafeParseSchema = { safeParse: (value: unknown) => { success: boolean } };

function createMockClient(): any {
  return {
    lsp: { status: async () => ({ data: [] }) },
    find: { symbols: async () => ({ data: [] }) },
  };
}

function createMockSdkContext(overrides: Partial<ToolContext> = {}): ToolContext {
  return {
    sessionID: "test-session",
    messageID: "test-message",
    agent: "test-agent",
    directory: PROJECT_CWD,
    worktree: PROJECT_CWD,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
    callID: "test-call",
    ...overrides,
  } as ToolContext;
}

function createHarness(
  sendImpl: (
    command: string,
    params: Record<string, unknown>,
    options?: BridgeRequestOptions & { onProgress?: ProgressHandler },
  ) => Promise<BridgeResponse> | BridgeResponse,
  triggerImpl?: PluginContext["plugin"],
) {
  const calls: SendCall[] = [];
  const bridge = {
    send: async (
      command: string,
      params: Record<string, unknown> = {},
      options?: BridgeRequestOptions & { onProgress?: ProgressHandler },
    ) => {
      calls.push({ command, params, options });
      return await sendImpl(command, params, options);
    },
  };
  const pool = { getBridge: () => bridge } as unknown as BridgePool;
  const ctx: PluginContext = {
    pool,
    client: createMockClient(),
    plugin: triggerImpl,
    config: {} as PluginContext["config"],
    storageDir: "/tmp/aft-test",
  };
  return { calls, tool: createBashTool(ctx) };
}

function safeParse(schema: unknown, value: unknown): { success: boolean } {
  return (schema as SafeParseSchema).safeParse(value);
}

describe("OpenCode bash adapter", () => {
  test("schema accepts valid unified bash params and rejects invalid shapes", () => {
    const { tool: bash } = createHarness(() => ({ success: true, output: "" }));

    expect(bash.description).toContain("By default, output is compressed");
    expect(bash.description).toContain("compressed: false");
    expect(bash.description).toContain("background: true");

    expect(safeParse(bash.args.command, "ls -la").success).toBe(true);
    expect(safeParse(bash.args.timeout, 120_000).success).toBe(true);
    expect(safeParse(bash.args.workdir, PROJECT_CWD).success).toBe(true);
    expect(safeParse(bash.args.description, "List files").success).toBe(true);
    expect(safeParse(bash.args.background, true).success).toBe(true);
    expect(safeParse(bash.args.compressed, false).success).toBe(true);

    expect(safeParse(bash.args.command, 123).success).toBe(false);
    expect(safeParse(bash.args.timeout, "slow").success).toBe(false);
    expect(safeParse(bash.args.background, "yes").success).toBe(false);
    expect(safeParse(bash.args.compressed, "no").success).toBe(false);

    for (const schema of Object.values(bash.args)) {
      const jsonSchema = tool.schema.toJSONSchema(schema) as { description?: string };
      expect(jsonSchema.description?.length).toBeGreaterThan(20);
    }
  });

  test("permission loop asks for each PermissionAsk and retries with permissions_granted", async () => {
    const ask = mock(async () => {});
    let sendCount = 0;
    const { calls, tool: bash } = createHarness((_command, _params, _options) => {
      sendCount++;
      if (sendCount === 1) {
        return {
          success: false,
          code: "permission_required",
          asks: [
            { kind: "bash", patterns: ["rm *"], always: ["rm *"] },
            { kind: "external_directory", patterns: ["/tmp/*"], always: [] },
          ],
        };
      }
      return { success: true, output: "ok", exit_code: 0, truncated: false };
    });

    await bash.execute({ command: "rm -rf /tmp/demo" }, createMockSdkContext({ ask }));

    expect(ask).toHaveBeenCalledTimes(2);
    expect(ask.mock.calls[0][0]).toEqual({
      permission: "bash",
      patterns: ["rm *"],
      always: ["rm *"],
      metadata: {},
    });
    expect(ask.mock.calls[1][0]).toEqual({
      permission: "external_directory",
      patterns: ["/tmp/*"],
      always: [],
      metadata: {},
    });
    expect(calls).toHaveLength(2);
    expect(calls[1].params.permissions_granted).toEqual(["rm *", "/tmp/*"]);
  });

  test("shell.env trigger fires before bridge call and merged env is forwarded", async () => {
    const events: string[] = [];
    const trigger = mock(async () => {
      events.push("trigger");
      return { env: { FOO: "bar", TOKEN: "redacted" } };
    });
    const { calls, tool: bash } = createHarness(
      () => {
        events.push("bridge");
        return { success: true, output: "env", exit_code: 0, truncated: false };
      },
      { trigger },
    );

    await bash.execute(
      { command: "printenv FOO", workdir: "/tmp/project" },
      createMockSdkContext({ sessionID: "s1", callID: "c1" } as Partial<ToolContext>),
    );

    expect(events).toEqual(["trigger", "bridge"]);
    expect(trigger).toHaveBeenCalledTimes(1);
    expect(trigger.mock.calls[0]).toEqual([
      "shell.env",
      { cwd: "/tmp/project", sessionID: "s1", callID: "c1" },
      { env: {} },
    ]);
    expect(calls[0].params.env).toEqual({ FOO: "bar", TOKEN: "redacted" });
  });

  test("large bash timeout scales bridge transport timeout with overhead", async () => {
    const { calls, tool: bash } = createHarness(() => ({
      success: true,
      output: "built",
      exit_code: 0,
      truncated: false,
    }));

    await bash.execute({ command: "cargo build", timeout: 600_000 }, createMockSdkContext());

    expect(calls).toHaveLength(1);
    expect(calls[0].params.timeout).toBe(600_000);
    expect(calls[0].options?.transportTimeoutMs).toBe(605_000);
  });

  test("progress callback forwards rolling output previews through ctx.metadata", async () => {
    const metadata = mock(() => {});
    const { tool: bash } = createHarness((_command, _params, options) => {
      options?.onProgress?.({ text: "hello " });
      options?.onProgress?.({ text: "world" });
      return { success: true, output: "hello world", exit_code: 0, truncated: false };
    });

    await bash.execute(
      { command: "printf hello", description: "Print greeting" },
      createMockSdkContext({ metadata }),
    );

    expect(metadata.mock.calls[0][0]).toEqual({ output: "hello ", description: "Print greeting" });
    expect(metadata.mock.calls[1][0]).toEqual({
      output: "hello world",
      description: "Print greeting",
    });
    expect(metadata.mock.calls.at(-1)?.[0]).toEqual({
      output: "hello world",
      description: "Print greeting",
      exit: 0,
      truncated: false,
    });
  });

  test("bg_completions are captured for notification hooks, not appended by bash adapter", async () => {
    const { tool: bash } = createHarness(() => ({
      success: true,
      output: "foreground",
      exit_code: 0,
      truncated: false,
      bg_completions: [
        { task_id: "abc123", status: "completed", exit_code: 0, command: "sleep 1; echo done" },
        { task_id: "xyz456", status: "killed", exit_code: null, command: "long-running script" },
      ],
    }));

    const output = await bash.execute({ command: "echo foreground" }, createMockSdkContext());

    expect(output).toBe("foreground");
  });

  test("truncation pointer and exit code are appended to agent-visible output, full payload stored as metadata", async () => {
    const { tool: bash } = createHarness(() => ({
      success: true,
      output: "done",
      exit_code: 0,
      truncated: true,
      output_path: "/tmp/bash-output.txt",
    }));

    const output = await bash.execute(
      { command: "echo done", description: "Echo done" },
      createMockSdkContext({
        sessionID: "meta-session",
        callID: "meta-call",
      } as Partial<ToolContext>),
    );
    const stored = consumeToolMetadata("meta-session", "meta-call");

    // Truncation must be visible to the agent (so it knows full output is on
    // disk); metadata payload preserves the structured fields for the UI.
    expect(output).toBe("done\n[output truncated; full output at /tmp/bash-output.txt]");
    expect(stored).toEqual({
      title: "Echo done",
      metadata: {
        description: "Echo done",
        output: "done",
        exit: 0,
        truncated: true,
        outputPath: "/tmp/bash-output.txt",
      },
    });
  });

  test("non-zero exit code is appended to agent-visible output", async () => {
    const { tool: bash } = createHarness(() => ({
      success: true,
      output: "command failed\n",
      exit_code: 2,
      truncated: false,
    }));

    const output = await bash.execute({ command: "false" }, createMockSdkContext());

    expect(output).toBe("command failed\n\n[exit code: 2]");
  });

  test("background spawn returns a concise started line and stores task metadata", async () => {
    const { tool: bash } = createHarness(() => ({
      success: true,
      status: "running",
      task_id: "task-xyz",
    }));

    const output = await bash.execute(
      { command: "sleep 30 && echo done", background: true },
      createMockSdkContext({
        sessionID: "bg-session",
        callID: "bg-call",
      } as Partial<ToolContext>),
    );
    const stored = consumeToolMetadata("bg-session", "bg-call");

    expect(output).toBe("Background task started: task-xyz");
    expect(stored?.metadata).toEqual({
      description: undefined,
      output: "Background task started: task-xyz",
      status: "running",
      taskId: "task-xyz",
    });
  });
});

describe("bash_status tool", () => {
  function makeCtx(sendImpl: (cmd: string, params: Record<string, unknown>) => BridgeResponse) {
    const bridge = {
      send: async (cmd: string, params: Record<string, unknown> = {}) => sendImpl(cmd, params),
    };
    const pool = { getBridge: () => bridge } as unknown as BridgePool;
    const ctx: PluginContext = {
      pool,
      client: createMockClient(),
      config: {} as PluginContext["config"],
      storageDir: "/tmp/aft-test",
    };
    return { ctx, statusTool: createBashStatusTool(ctx), killTool: createBashKillTool(ctx) };
  }

  test("returns running status with no output preview", async () => {
    const { statusTool } = makeCtx((_cmd, _params) => ({
      success: true,
      status: "running",
      exit_code: null,
      duration_ms: 3000,
      output_preview: null,
    }));
    const result = await statusTool.execute({ taskId: "bgb-abc123" }, createMockSdkContext());
    expect(result).toBe("Task bgb-abc123: running 3s");
    expect(result).not.toContain("null");
  });

  test("returns completed status with exit code and output preview", async () => {
    const { statusTool } = makeCtx((_cmd, _params) => ({
      success: true,
      status: "completed",
      exit_code: 0,
      duration_ms: 15168,
      output_preview: "test 1: bg starting at 09:19:24\ntest 1: bg done at 09:19:39",
    }));
    const result = await statusTool.execute({ taskId: "bgb-6b454047" }, createMockSdkContext());
    expect(result).toContain("Task bgb-6b454047: completed (exit 0) 15s");
    expect(result).toContain("test 1: bg starting at");
    expect(result).toContain("test 1: bg done at");
  });

  test("forwards task_id as snake_case to bridge", async () => {
    const calls: Array<{ cmd: string; params: Record<string, unknown> }> = [];
    const { statusTool } = makeCtx((cmd, params) => {
      calls.push({ cmd, params });
      return { success: true, status: "running", exit_code: null, duration_ms: 0 };
    });
    await statusTool.execute({ taskId: "bgb-deadbeef" }, createMockSdkContext());
    expect(calls[0].cmd).toBe("bash_status");
    expect(calls[0].params.task_id).toBe("bgb-deadbeef");
  });

  test("throws on bridge error", async () => {
    const { statusTool } = makeCtx(() => ({
      success: false,
      code: "not_found",
      message: "task bgb-unknown not found",
    }));
    await expect(
      statusTool.execute({ taskId: "bgb-unknown" }, createMockSdkContext()),
    ).rejects.toThrow("task bgb-unknown not found");
  });

  test("bash_kill forwards task_id and returns confirmation", async () => {
    const calls: Array<{ cmd: string; params: Record<string, unknown> }> = [];
    const { killTool } = makeCtx((cmd, params) => {
      calls.push({ cmd, params });
      return { success: true, status: "killed" };
    });
    const result = await killTool.execute({ taskId: "bgb-deadbeef" }, createMockSdkContext());
    expect(result).toBe("Task bgb-deadbeef: killed");
    expect(calls[0].cmd).toBe("bash_kill");
    expect(calls[0].params.task_id).toBe("bgb-deadbeef");
  });

  test("bash_kill surfaces already-terminal status from bridge", async () => {
    const { killTool } = makeCtx(() => ({ success: true, status: "completed", exit_code: 0 }));
    const result = await killTool.execute({ taskId: "bgb-done" }, createMockSdkContext());
    expect(result).toBe("Task bgb-done: completed");
  });

  test("bash_kill throws on bridge error", async () => {
    const { killTool } = makeCtx(() => ({
      success: false,
      code: "not_running",
      message: "task already finished",
    }));
    await expect(killTool.execute({ taskId: "bgb-done" }, createMockSdkContext())).rejects.toThrow(
      "task already finished",
    );
  });
});
