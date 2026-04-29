/**
 * Unit tests for the Pi bash tool adapter.
 *
 * Covers:
 * - Schema validation (required command, optional fields)
 * - BashSpawnHook invocation
 * - Progress callback handling
 * - background task metadata tracking
 */

import { describe, expect, test } from "bun:test";
import type { ExtensionAPI, Theme } from "@mariozechner/pi-coding-agent";
import { Container, Text } from "@mariozechner/pi-tui";
import type { BinaryBridge } from "../bridge.js";
import { registerBashTool } from "../tools/bash.js";
import type { PluginContext } from "../types.js";

// Minimal mock types
interface MockToolDef {
  name: string;
  label: string;
  description: string;
  parameters: unknown;
  execute: (
    toolCallId: string,
    params: unknown,
    signal: AbortSignal | undefined,
    onUpdate: ((update: unknown) => void) | undefined,
    ctx: { cwd: string },
  ) => Promise<unknown>;
  renderCall?: (args: unknown, theme: Theme, context: unknown) => unknown;
  renderResult?: (result: unknown, options: unknown, theme: Theme, context: unknown) => unknown;
}

interface MockExtensionContext {
  cwd: string;
  hasUI: boolean;
}

// Mock theme for renderer tests
const mockTheme: Theme = {
  fg: (color: string, text: string) => `[${color}]${text}[/${color}]`,
  bold: (text: string) => `**${text}**`,
} as unknown as Theme;

// Build a minimal mock ExtensionAPI that captures registered tools
function makeMockApi(tools: Map<string, MockToolDef>): ExtensionAPI {
  return {
    registerTool: (tool: MockToolDef) => {
      tools.set(tool.name, tool);
    },
  } as unknown as ExtensionAPI;
}

// Mock bridge that captures calls and returns configurable responses
function makeMockBridge(response: Record<string, unknown> = {}): BinaryBridge {
  const sendFn = async () => ({ success: true, ...response });
  return {
    send: sendFn,
  } as unknown as BinaryBridge;
}

// Trackable mock bridge for verifying calls
function makeTrackableMockBridge(response: Record<string, unknown> = {}): {
  bridge: BinaryBridge;
  calls: unknown[];
} {
  const calls: unknown[] = [];
  const bridge = {
    send: async (...args: unknown[]) => {
      calls.push(args);
      return { success: true, ...response };
    },
  } as unknown as BinaryBridge;
  return { bridge, calls };
}

// Mock plugin context
function makeMockContext(bridge: BinaryBridge): PluginContext {
  return {
    pool: {
      getBridge: () => bridge,
    } as unknown as PluginContext["pool"],
    config: {} as PluginContext["config"],
    storageDir: "/tmp/test",
  };
}

describe("bash tool adapter", () => {
  test("schema has comprehensive descriptions", () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const mockBridge = makeMockBridge();
    const ctx = makeMockContext(mockBridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash");
    expect(bashTool).toBeDefined();

    // Tool description mentions compressed and background options
    expect(bashTool!.description).toContain("compressed");
    expect(bashTool!.description).toContain("background");
    expect(bashTool!.description).toContain("task_id");
  });

  test("execute calls bridge with correct parameters", async () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const { bridge, calls } = makeTrackableMockBridge({
      output: "hello world",
      exit_code: 0,
      duration_ms: 100,
    });
    const ctx = makeMockContext(bridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    const result = (await bashTool.execute(
      "test-call",
      { command: "echo hello" },
      undefined,
      undefined,
      extCtx,
    )) as { content: Array<{ type: string; text: string }>; details: Record<string, unknown> };

    // Verify bridge was called
    expect(calls.length).toBe(1);

    // Check the command parameter
    const callArgs = calls[0] as [string, Record<string, unknown>];
    expect(callArgs[0]).toBe("bash");
    expect(callArgs[1].command).toBe("echo hello");

    // Verify result structure
    expect(result.content[0].text).toBe("hello world");
    expect(result.details.exit_code).toBe(0);
    expect(result.details.duration_ms).toBe(100);
  });

  test("BashSpawnHook modifies command before bridge call", async () => {
    const tools = new Map<string, MockToolDef>();

    // Create API with a BashSpawnHook
    const hookCalls: Array<{ command: string; cwd?: string }> = [];
    const apiWithHook = {
      registerTool: (tool: MockToolDef) => {
        tools.set(tool.name, tool);
      },
      getHook: (name: string) => {
        if (name === "bashSpawn") {
          return async (ctx: { command: string; cwd?: string }) => {
            hookCalls.push(ctx);
            return {
              command: `modified: ${ctx.command}`,
              cwd: "/modified/cwd",
              env: { TEST_VAR: "value" },
            };
          };
        }
        return undefined;
      },
    } as unknown as ExtensionAPI;

    const { bridge, calls } = makeTrackableMockBridge({ output: "result" });
    const ctx = makeMockContext(bridge);

    registerBashTool(apiWithHook, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    await bashTool.execute(
      "test-call",
      { command: "original command", workdir: "/original" },
      undefined,
      undefined,
      extCtx,
    );

    // Verify hook was called with original params
    expect(hookCalls.length).toBe(1);
    expect(hookCalls[0].command).toBe("original command");
    expect(hookCalls[0].cwd).toBe("/original");

    // Verify bridge received modified params
    const callArgs = calls[0] as [string, Record<string, unknown>];
    expect(callArgs[1].command).toBe("modified: original command");
    expect(callArgs[1].workdir).toBe("/modified/cwd");
    expect(callArgs[1].env).toEqual({ TEST_VAR: "value" });
  });

  test("BashSpawnHook errors are surfaced", async () => {
    const tools = new Map<string, MockToolDef>();

    const apiWithFailingHook = {
      registerTool: (tool: MockToolDef) => {
        tools.set(tool.name, tool);
      },
      getHook: () => {
        return async () => {
          throw new Error("Hook failed: permission denied");
        };
      },
    } as unknown as ExtensionAPI;

    const mockBridge = makeMockBridge();
    const ctx = makeMockContext(mockBridge);

    registerBashTool(apiWithFailingHook, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    await expect(
      bashTool.execute("test-call", { command: "echo test" }, undefined, undefined, extCtx),
    ).rejects.toThrow("Hook failed: permission denied");
  });

  test("execute throws Rust-side bash error responses", async () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const mockBridge = makeMockBridge({
      success: false,
      code: "execution_failed",
      message: "kaboom",
    });
    const ctx = makeMockContext(mockBridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    await expect(
      bashTool.execute("test-call", { command: "boom" }, undefined, undefined, extCtx),
    ).rejects.toThrow("kaboom");
  });

  test("progress callback streams output", async () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);

    // Track progress callbacks
    const progressCallbacks: Array<{ text: string }> = [];

    // Bridge that simulates progress callbacks
    // callBridge passes options as 3rd argument to bridge.send
    const mockBridge = {
      send: async (
        _cmd: string,
        _params: unknown,
        options?: { onProgress?: (chunk: { kind: string; text: string }) => void },
      ) => {
        // Simulate progress
        if (options?.onProgress) {
          options.onProgress({ kind: "stdout", text: "line1\n" });
          options.onProgress({ kind: "stdout", text: "line2\n" });
          progressCallbacks.push({ text: "line1\n" }, { text: "line2\n" });
        }
        return { success: true, output: "final output", exit_code: 0 };
      },
    } as unknown as BinaryBridge;

    const ctx = makeMockContext(mockBridge);
    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    const updates: unknown[] = [];
    const result = await bashTool.execute(
      "test-call",
      { command: "long running" },
      undefined,
      (update) => updates.push(update),
      extCtx,
    );

    // Verify progress callbacks were invoked
    expect(progressCallbacks.length).toBe(2);

    // Verify final result has the output
    const finalResult = result as { content: Array<{ text: string }> };
    expect(finalResult.content[0].text).toContain("final output");
  });

  test("bg_completions metadata is not appended by bash adapter", async () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const mockBridge = makeMockBridge({
      output: "Main output",
      exit_code: 0,
      bg_completions: [
        { task_id: "bg-1", status: "completed", exit_code: 0, command: "npm install" },
        { task_id: "bg-2", status: "failed", exit_code: 1, command: "npm run build" },
      ],
    });
    const ctx = makeMockContext(mockBridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    const result = (await bashTool.execute(
      "test-call",
      { command: "main command" },
      undefined,
      undefined,
      extCtx,
    )) as {
      content: Array<{ type: string; text: string }>;
      details: { bg_completions?: Array<{ task_id: string }> };
    };

    expect(result.details.bg_completions).toBeUndefined();
    expect(result.content[0].text).toBe("Main output");
  });

  test("permission_required error throws clear message", async () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);

    const mockBridge = {
      send: async () => {
        throw new Error("permission_required: bash command requires permission");
      },
    } as unknown as BinaryBridge;

    const ctx = makeMockContext(mockBridge);
    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    await expect(
      bashTool.execute("test-call", { command: "rm -rf /" }, undefined, undefined, extCtx),
    ).rejects.toThrow("Permission ask reached Pi adapter");
  });

  test("renderCall returns Text component", () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const mockBridge = makeMockBridge();
    const ctx = makeMockContext(mockBridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    expect(bashTool.renderCall).toBeDefined();

    // With description
    const withDesc = bashTool.renderCall!(
      { command: "echo test", description: "Print greeting" },
      mockTheme,
      { lastComponent: undefined, isError: false },
    );
    expect(withDesc).toBeInstanceOf(Text);

    // With long command (should be shortened)
    const longCmd = "a".repeat(100);
    const withLongCmd = bashTool.renderCall!({ command: longCmd }, mockTheme, {
      lastComponent: undefined,
      isError: false,
    });
    expect(withLongCmd).toBeInstanceOf(Text);
  });

  test("renderResult returns appropriate component types", () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const mockBridge = makeMockBridge();
    const ctx = makeMockContext(mockBridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    expect(bashTool.renderResult).toBeDefined();

    // Success result with bg_completions
    const successResult = {
      content: [{ type: "text", text: "output" }],
      details: {
        exit_code: 0,
        duration_ms: 150,
        bg_completions: [
          { task_id: "task-1", status: "completed", exit_code: 0, command: "npm install" },
        ],
      },
    };

    const rendered = bashTool.renderResult!(successResult, {}, mockTheme, {
      lastComponent: undefined,
      isError: false,
    });

    expect(rendered).toBeInstanceOf(Container);

    // Error result
    const errorResult = {
      content: [{ type: "text", text: "Command failed" }],
      details: { exit_code: 1 },
    };

    const errorRendered = bashTool.renderResult!(errorResult, {}, mockTheme, {
      lastComponent: undefined,
      isError: true,
    });

    expect(errorRendered).toBeInstanceOf(Text);
  });

  test("handles missing bg_completions gracefully", async () => {
    const tools = new Map<string, MockToolDef>();
    const api = makeMockApi(tools);
    const mockBridge = makeMockBridge({
      output: "Simple output",
      exit_code: 0,
      // No bg_completions field
    });
    const ctx = makeMockContext(mockBridge);

    registerBashTool(api, ctx);

    const bashTool = tools.get("bash")!;
    const extCtx: MockExtensionContext = { cwd: "/test", hasUI: false };

    const result = (await bashTool.execute(
      "test-call",
      { command: "echo test" },
      undefined,
      undefined,
      extCtx,
    )) as { content: Array<{ text: string }>; details: { bg_completions?: unknown[] } };

    // Should not have bg_completions in details
    expect(result.details.bg_completions).toBeUndefined();

    // Text should not contain background task notifications
    expect(result.content[0].text).toBe("Simple output");
  });
});
