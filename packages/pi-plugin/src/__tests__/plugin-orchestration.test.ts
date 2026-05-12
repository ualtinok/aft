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
});
