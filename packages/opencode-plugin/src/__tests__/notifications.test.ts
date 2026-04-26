/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type ConfigureWarning, deliverConfigureWarnings } from "../notifications.js";

const tempRoots = new Set<string>();

function createStorageDir(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-opencode-notifications-"));
  tempRoots.add(root);
  return root;
}

function createClient() {
  const messages: string[] = [];
  const client = {
    session: {
      prompt(input: { body?: { parts?: Array<{ text?: string }> } }): void {
        const text = input.body?.parts?.[0]?.text;
        if (text) messages.push(text);
      },
    },
  };
  return { client, messages };
}

function baseWarning(overrides: Partial<ConfigureWarning> = {}): ConfigureWarning {
  return {
    kind: "formatter_not_installed",
    language: "typescript",
    tool: "biome",
    hint: "Install biome with bun add -d @biomejs/biome.",
    ...overrides,
  };
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("deliverConfigureWarnings", () => {
  test("first-time warning delivers via sendIgnoredMessage", async () => {
    const storageDir = createStorageDir();
    const { client, messages } = createClient();

    await deliverConfigureWarnings(
      { client, sessionId: "session-1", storageDir, pluginVersion: "1.0.0", projectRoot: "/repo" },
      [baseWarning()],
    );

    expect(messages).toHaveLength(1);
    expect(messages[0]).toContain("🔧 AFT: ⚠️");
    expect(messages[0]).toContain("Formatter is not installed");
    expect(messages[0]).toContain("Install biome");
  });

  test("second call with same warning skips delivery", async () => {
    const storageDir = createStorageDir();
    const { client, messages } = createClient();
    const opts = {
      client,
      sessionId: "session-1",
      storageDir,
      pluginVersion: "1.0.0",
      projectRoot: "/repo",
    };

    await deliverConfigureWarnings(opts, [baseWarning()]);
    await deliverConfigureWarnings(opts, [baseWarning()]);

    expect(messages).toHaveLength(1);
  });

  test("different warnings deliver independently", async () => {
    const storageDir = createStorageDir();
    const { client, messages } = createClient();

    await deliverConfigureWarnings(
      { client, sessionId: "session-1", storageDir, pluginVersion: "1.0.0", projectRoot: "/repo" },
      [
        baseWarning(),
        baseWarning({ kind: "checker_not_installed", tool: "tsc", hint: "Install typescript." }),
      ],
    );

    expect(messages).toHaveLength(2);
    expect(messages[0]).toContain("Formatter is not installed");
    expect(messages[1]).toContain("Checker is not installed");
  });

  test("plugin version bump does not re-fire stale warnings", async () => {
    const storageDir = createStorageDir();
    const { client, messages } = createClient();

    await deliverConfigureWarnings(
      { client, sessionId: "session-1", storageDir, pluginVersion: "1.0.0", projectRoot: "/repo" },
      [baseWarning()],
    );
    await deliverConfigureWarnings(
      { client, sessionId: "session-1", storageDir, pluginVersion: "2.0.0", projectRoot: "/repo" },
      [baseWarning()],
    );

    expect(messages).toHaveLength(1);
    const persisted = JSON.parse(readFileSync(join(storageDir, "warned_tools.json"), "utf-8"));
    expect(Object.values(persisted)).toEqual(["1.0.0"]);
  });

  test("file corruption and missing storage_dir are non-fatal", async () => {
    const storageDir = createStorageDir();
    writeFileSync(join(storageDir, "warned_tools.json"), "not json");
    const missingStorageDir = join(storageDir, "missing", "nested");
    const { client, messages } = createClient();

    await deliverConfigureWarnings(
      { client, sessionId: "session-1", storageDir, pluginVersion: "1.0.0", projectRoot: "/repo" },
      [baseWarning()],
    );
    await deliverConfigureWarnings(
      {
        client,
        sessionId: "session-1",
        storageDir: missingStorageDir,
        pluginVersion: "1.0.0",
        projectRoot: "/repo",
      },
      [baseWarning({ tool: "prettier", hint: "Install prettier." })],
    );

    expect(messages).toHaveLength(2);
  });
});
