/// <reference path="../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { __test__ } from "../index.js";

describe("Pi Lane G plugin orchestration regressions", () => {
  test("eager configure warnings buffer and flush exactly once on first session-bound call", async () => {
    const root = mkdtempSync(join(tmpdir(), "aft-pi-eager-warnings-"));
    const messages: string[] = [];
    const client = { ui: { notify: (message: string) => messages.push(message) } };
    const warning = {
      kind: "formatter_not_installed" as const,
      language: "ts",
      tool: "biome",
      hint: "Install biome.",
    };
    try {
      await __test__.handleConfigureWarningsForSession({
        projectRoot: "/repo-pi-eager",
        warnings: [warning],
        storageDir: root,
        pluginVersion: "1.0.0",
      });
      expect(messages).toHaveLength(0);

      await __test__.handleConfigureWarningsForSession({
        projectRoot: "/repo-pi-eager",
        sessionId: "session-1",
        client,
        warnings: [],
        storageDir: root,
        pluginVersion: "1.0.0",
      });
      await __test__.handleConfigureWarningsForSession({
        projectRoot: "/repo-pi-eager",
        sessionId: "session-1",
        client,
        warnings: [],
        storageDir: root,
        pluginVersion: "1.0.0",
      });

      expect(messages).toHaveLength(1);
      expect(messages[0]).toContain("Formatter is not installed");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("ONNX runtime is only prepared for fastembed semantic search", () => {
    expect(__test__.shouldPrepareOnnxRuntime({ semantic_search: true })).toBe(true);
    expect(
      __test__.shouldPrepareOnnxRuntime({
        semantic_search: true,
        semantic: { backend: "fastembed" },
      }),
    ).toBe(true);
    expect(
      __test__.shouldPrepareOnnxRuntime({
        semantic_search: true,
        semantic: { backend: "openai_compatible" },
      }),
    ).toBe(false);
    expect(
      __test__.shouldPrepareOnnxRuntime({
        semantic_search: true,
        semantic: { backend: "ollama" },
      }),
    ).toBe(false);
    expect(__test__.shouldPrepareOnnxRuntime({ semantic_search: false })).toBe(false);
  });

  test("version mismatch handler downloads matching binary and hot-swaps pool", async () => {
    const ensureCalls: string[] = [];
    const replaceCalls: string[] = [];
    const handler = __test__.createVersionMismatchHandler(
      () => ({
        replaceBinary: async (path: string) => {
          replaceCalls.push(path);
        },
      }),
      async (version?: string) => {
        ensureCalls.push(version ?? "");
        return "/cache/aft/v1.2.3/aft";
      },
    );

    handler("1.0.0", "1.2.3");
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(ensureCalls).toEqual(["v1.2.3"]);
    expect(replaceCalls).toEqual(["/cache/aft/v1.2.3/aft"]);
  });

  test("version mismatch handler only attempts one hot-swap per stale binary version", async () => {
    const ensureCalls: string[] = [];
    const handler = __test__.createVersionMismatchHandler(
      () => ({ replaceBinary: async () => {} }),
      async (version?: string) => {
        ensureCalls.push(version ?? "");
        return null;
      },
    );

    handler("1.0.0", "1.2.3");
    handler("1.0.0", "1.2.3");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(ensureCalls).toEqual(["v1.2.3"]);
  });
});
