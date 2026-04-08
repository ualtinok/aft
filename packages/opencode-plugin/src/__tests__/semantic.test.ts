/// <reference path="../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import type { ToolContext } from "@opencode-ai/plugin";
import type { BridgePool } from "../pool.js";
import { semanticTools } from "../tools/semantic.js";
import type { PluginContext } from "../types.js";

type BridgeResponse = Record<string, unknown>;
type SendCall = { command: string; params: Record<string, unknown> };
type BridgeCall = { directory: string; sessionID: string };

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
  return { pool, client: createMockClient(), config: config as PluginContext["config"] };
}

function createMockSdkContext(directory = "/tmp/semantic-tests"): ToolContext {
  return {
    sessionID: "semantic-session",
    messageID: "message-id",
    agent: "test",
    directory,
    worktree: directory,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
  };
}

function createMockSemanticHarness(
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
    getBridge: (directory: string, sessionID: string) => {
      bridgeCalls.push({ directory, sessionID });
      return bridge;
    },
  } as unknown as BridgePool;

  return {
    bridgeCalls,
    sendCalls,
    tools: semanticTools(createPluginContext(pool, config)),
  };
}

describe("semanticTools", () => {
  test("registers aft_search", () => {
    const { tools } = createMockSemanticHarness({}, () => ({ success: true }));

    expect(Object.keys(tools)).toEqual(["aft_search"]);
  });

  test("returns response.text and sends semantic_search params", async () => {
    const sdkCtx = createMockSdkContext("/tmp/project");
    const { bridgeCalls, sendCalls, tools } = createMockSemanticHarness({}, () => ({
      success: true,
      text: "src/auth.ts\nvalidateToken [function] lines 10-32 score 0.913",
    }));

    const output = await tools.aft_search.execute(
      { query: "authentication logic", topK: 5 },
      sdkCtx,
    );

    expect(bridgeCalls).toEqual([{ directory: "/tmp/project", sessionID: "semantic-session" }]);
    expect(sendCalls).toEqual([
      {
        command: "semantic_search",
        params: {
          query: "authentication logic",
          top_k: 5,
        },
      },
    ]);
    expect(output).toBe("src/auth.ts\nvalidateToken [function] lines 10-32 score 0.913");
  });
});
