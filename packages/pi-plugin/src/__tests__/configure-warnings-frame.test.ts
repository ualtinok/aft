/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { handleConfigureWarningsForSession } from "../index.js";

const tempRoots = new Set<string>();

function createStorageDir(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-pi-configure-warnings-"));
  tempRoots.add(root);
  return root;
}

function createClient() {
  const messages: string[] = [];
  const client = {
    ui: {
      notify(message: string): void {
        messages.push(message);
      },
    },
  };
  return { client, messages };
}

function baseWarning() {
  return {
    kind: "formatter_not_installed",
    language: "typescript",
    tool: "biome",
    hint: "Install biome with bun add -d @biomejs/biome.",
  };
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("configure_warnings push-frame handler", () => {
  test("delivers a valid session_id to that session's notification handler", async () => {
    const storageDir = createStorageDir();
    const { client, messages } = createClient();

    await handleConfigureWarningsForSession({
      projectRoot: "/repo",
      sessionId: "session-1",
      client,
      warnings: [baseWarning()],
      storageDir,
      pluginVersion: "1.0.0",
    });

    expect(messages).toHaveLength(1);
    expect(messages[0]).toContain("Formatter is not installed");
  });

  test("missing session_id falls back gracefully without crashing", async () => {
    const storageDir = createStorageDir();
    const { client, messages } = createClient();

    await expect(
      handleConfigureWarningsForSession({
        projectRoot: "/repo",
        sessionId: null,
        client,
        warnings: [baseWarning()],
        storageDir,
        pluginVersion: "1.0.0",
      }),
    ).resolves.toBeUndefined();

    expect(messages).toHaveLength(0);
  });
});
