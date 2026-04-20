/// <reference path="../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import type { ToolContext } from "@opencode-ai/plugin";
import type { BridgePool } from "../pool.js";
import { searchTools } from "../tools/search.js";
import type { PluginContext } from "../types.js";

type BridgeResponse = Record<string, unknown>;
type SendCall = { command: string; params: Record<string, unknown> };
type BridgeCall = { projectRoot: string };

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

function createPluginContext(pool: BridgePool, config: Record<string, unknown>): PluginContext {
  return {
    pool,
    client: createMockClient(),
    config: config as PluginContext["config"],
    storageDir: "/tmp/aft-test",
  };
}

function createMockSdkContext(directory = "/tmp/search-tests"): ToolContext {
  return {
    sessionID: "search-session",
    messageID: "message-id",
    agent: "test",
    directory,
    worktree: directory,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
  };
}

function createMockSearchHarness(
  config: Record<string, unknown>,
  sendImpl: (
    command: string,
    params: Record<string, unknown>,
  ) => Promise<BridgeResponse> | BridgeResponse,
) {
  const sendCalls: SendCall[] = [];
  const bridgeCalls: BridgeCall[] = [];
  const bridge = {
    send: async (command: string, params: Record<string, unknown> = {}) => {
      sendCalls.push({ command, params });
      return await sendImpl(command, params);
    },
  };

  const pool = {
    getBridge: (projectRoot: string) => {
      bridgeCalls.push({ projectRoot });
      return bridge;
    },
  } as unknown as BridgePool;

  return {
    bridgeCalls,
    sendCalls,
    tools: searchTools(createPluginContext(pool, config)),
  };
}

describe("searchTools", () => {
  test("registers hoisted tool names when built-in hoisting is enabled", () => {
    const { tools } = createMockSearchHarness({ hoist_builtin_tools: true }, () => ({
      success: true,
    }));

    expect(Object.keys(tools).sort()).toEqual(["glob", "grep"]);
  });

  test("registers aft-prefixed tool names when built-in hoisting is disabled", () => {
    const { tools } = createMockSearchHarness({ hoist_builtin_tools: false }, () => ({
      success: true,
    }));

    expect(Object.keys(tools).sort()).toEqual(["aft_glob", "aft_grep"]);
  });

  test("returns grep response.text when provided and uses session-scoped bridges", async () => {
    const sdkCtx = createMockSdkContext("/tmp/project");
    const { bridgeCalls, sendCalls, tools } = createMockSearchHarness(
      { hoist_builtin_tools: true },
      () => ({
        success: true,
        text: [
          "── src/main.rs (2 matches) ──",
          "  42: fn dispatch(req: RawRequest, ctx: &AppContext) -> Response {",
          "  80: fn dispatch(req: RawRequest, ctx: &AppContext) -> Response {",
          "",
          "Found 2 match(es) across 1 file(s). [index: ready]",
        ].join("\n"),
      }),
    );

    const output = await tools.grep.execute({ pattern: "dispatch" }, sdkCtx);

    // Bridge is project-keyed now; sessionID travels in params via callBridge.
    expect(bridgeCalls.length).toBe(1);
    expect(sendCalls).toHaveLength(1);
    expect(sendCalls[0]?.command).toBe("grep");
    expect(sendCalls[0]?.params).toEqual({
      pattern: "dispatch",
      case_sensitive: true,
      include: undefined,
      path: undefined,
      max_results: 100,
      session_id: "search-session",
    });
    expect(output).toBe(
      [
        "── src/main.rs (2 matches) ──",
        "  42: fn dispatch(req: RawRequest, ctx: &AppContext) -> Response {",
        "  80: fn dispatch(req: RawRequest, ctx: &AppContext) -> Response {",
        "",
        "Found 2 match(es) across 1 file(s). [index: ready]",
      ].join("\n"),
    );
  });

  test("returns glob response.text when provided", async () => {
    const { tools } = createMockSearchHarness({ hoist_builtin_tools: true }, () => ({
      success: true,
      text: [
        "21 files matching src/**/*.ts",
        "",
        "src/ (21 files)",
        "  one.ts, two.ts, three.ts, four.ts, five.ts, ...",
      ].join("\n"),
      files: ["src/one.ts", "src/two.ts"],
    }));

    const output = await tools.glob.execute({ pattern: "src/**/*.ts" }, createMockSdkContext());

    expect(output).toBe(
      [
        "21 files matching src/**/*.ts",
        "",
        "src/ (21 files)",
        "  one.ts, two.ts, three.ts, four.ts, five.ts, ...",
      ].join("\n"),
    );
  });

  test("falls back to newline-joined glob paths when text is unavailable", async () => {
    const { tools } = createMockSearchHarness({ hoist_builtin_tools: true }, () => ({
      success: true,
      files: ["src/one.ts", "src/two.ts"],
    }));

    const output = await tools.glob.execute({ pattern: "src/**/*.ts" }, createMockSdkContext());

    expect(output).toBe(["src/one.ts", "src/two.ts"].join("\n"));
  });
});
