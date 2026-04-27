/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import type { HarnessAdapter, HarnessConfigPaths } from "../adapters/types.js";
import {
  clearDoctorCaches,
  DOCTOR_CLEAR_TARGET_OPTIONS,
  DOCTOR_FORCE_CLEAR_TARGETS,
  type DoctorClearTarget,
} from "../commands/doctor.js";

function makeAdapter(overrides: Partial<HarnessAdapter> = {}): HarnessAdapter {
  const configPaths: HarnessConfigPaths = {
    configDir: "/tmp/aft-test",
    harnessConfig: "/tmp/aft-test/opencode.jsonc",
    harnessConfigFormat: "jsonc",
    aftConfig: "/tmp/aft-test/aft.jsonc",
    aftConfigFormat: "jsonc",
  };

  return {
    kind: "opencode",
    displayName: "OpenCode",
    pluginPackageName: "@cortexkit/aft-opencode",
    pluginEntryWithVersion: "@cortexkit/aft-opencode@latest",
    isInstalled: () => true,
    getHostVersion: () => "test",
    detectConfigPaths: () => configPaths,
    hasPluginEntry: () => true,
    ensurePluginEntry: async () => ({
      ok: true,
      action: "already_present",
      message: "already registered",
      configPath: configPaths.harnessConfig,
    }),
    getPluginCacheInfo: () => ({
      path: "/tmp/aft-test/plugin-cache",
      exists: false,
    }),
    getStorageDir: () => "/tmp/aft-test/storage",
    getLogFile: () => "/tmp/aft-test/aft.log",
    getInstallHint: () => "Install OpenCode",
    clearPluginCache: async () => ({
      action: "not_found",
      path: "/tmp/aft-test/plugin-cache",
    }),
    ...overrides,
  };
}

describe("doctor cache clear targets", () => {
  test("lists the interactive clear categories in prompt order", () => {
    expect(DOCTOR_CLEAR_TARGET_OPTIONS).toEqual([
      {
        label: "Plugin npm cache (~/.cache/opencode/packages/@cortexkit/aft-opencode@latest, etc.)",
        value: "plugin-cache",
      },
      {
        label: "LSP install cache (~/.cache/aft/lsp-packages/, ~/.cache/aft/lsp-binaries/)",
        value: "lsp-cache",
      },
    ]);
  });

  test("keeps --force as a plugin-cache-only clear target", () => {
    expect(DOCTOR_FORCE_CLEAR_TARGETS satisfies DoctorClearTarget[]).toEqual(["plugin-cache"]);
  });
});

describe("clearDoctorCaches", () => {
  test("--force compatibility clears plugin cache and does not touch LSP cache", async () => {
    let pluginClears = 0;
    let lspClears = 0;
    const adapter = makeAdapter({
      clearPluginCache: async () => {
        pluginClears += 1;
        return {
          action: "cleared",
          path: "/tmp/aft-test/plugin-cache",
        };
      },
    });

    const summary = await clearDoctorCaches([adapter], DOCTOR_FORCE_CLEAR_TARGETS, {
      clearLspCaches: () => {
        lspClears += 1;
        return { cleared: [], errors: [], totalBytes: 0 };
      },
      includePluginBytes: false,
    });

    expect(pluginClears).toBe(1);
    expect(lspClears).toBe(0);
    expect(summary.pluginCache).toEqual({ cleared: 1, totalBytes: 0, errors: 0 });
    expect(summary.lspCache).toBeUndefined();
    expect(summary.hadErrors).toBe(false);
  });

  test("selected LSP cache clears without requiring plugin-cache adapters", async () => {
    let lspClears = 0;

    const summary = await clearDoctorCaches([], ["lsp-cache"], {
      clearLspCaches: () => {
        lspClears += 1;
        return {
          cleared: [{ name: "pyright", path: "/tmp/aft-test/lsp-packages/pyright", size: 2048 }],
          errors: [],
          totalBytes: 2048,
        };
      },
    });

    expect(lspClears).toBe(1);
    expect(summary.pluginCache).toBeUndefined();
    expect(summary.lspCache).toEqual({ cleared: 1, totalBytes: 2048, errors: 0 });
    expect(summary.hadErrors).toBe(false);
  });
});
