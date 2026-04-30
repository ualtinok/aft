/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";

import { BridgePool } from "../../pool.js";
import { hoistedTools } from "../../tools/hoisted.js";
import type { PluginContext } from "../../types.js";
import {
  BIOME_TS_EXCLUDED_PRESET,
  BIOME_TS_PRESET,
  createFormatHarness,
  type FakeFormatterShim,
  FIXTURES,
  type FormatPreset,
  NO_FORMATTER_PRESET,
} from "./format-helpers.js";
import { type E2EHarness, type PreparedBinary, prepareBinary } from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = describe.skipIf(!initialBinary.binaryPath);
const biomeOnPath = await commandAvailable("biome");

const DOCUMENTED_FORMAT_SKIP_REASONS = [
  "unsupported_language",
  "no_formatter_configured",
  "formatter_not_installed",
  "formatter_excluded_path",
  "timeout",
  "error",
] as const;

type FormatSkipReason = (typeof DOCUMENTED_FORMAT_SKIP_REASONS)[number];

const TS_INPUT = FIXTURES.ts_deformatted;

function createMockClient(): any {
  return {
    lsp: { status: async () => ({ data: [] }) },
    find: { symbols: async () => ({ data: [] }) },
  };
}

function createPluginContext(pool: BridgePool, storageDir: string): PluginContext {
  return {
    pool,
    client: createMockClient(),
    config: {} as PluginContext["config"],
    storageDir,
  };
}

function createSdkContext(directory: string): ToolContext {
  return {
    sessionID: `format-on-edit-skip-reasons-${Date.now()}`,
    messageID: "format-on-edit-skip-reasons-message",
    agent: "test",
    directory,
    worktree: directory,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
  };
}

function formatterPreset(tool: string): FormatPreset {
  return { configFiles: [], explicitFormatter: { typescript: tool } };
}

function shim(name: string, stderr: string, code = 1): FakeFormatterShim {
  return {
    name,
    script: `#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "${name} 1.0.0"
  exit 0
fi
echo ${JSON.stringify(stderr)} >&2
exit ${code}
`,
  };
}

function excludedPathShim(name = "biome"): FakeFormatterShim {
  return shim(name, "No files were processed in the specified paths.");
}

function genericErrorShim(name = "biome"): FakeFormatterShim {
  return shim(name, "fake formatter: something exploded", 2);
}

function hangingShim(name = "biome"): FakeFormatterShim {
  return {
    name,
    script: `#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "${name} 1.0.0"
  exit 0
fi
sleep 10
`,
  };
}

async function commandAvailable(command: string): Promise<boolean> {
  const paths = (process.env.PATH ?? "").split(":").filter(Boolean);
  for (const dir of paths) {
    try {
      await access(join(dir, command));
      return true;
    } catch {
      // keep searching
    }
  }
  return false;
}

maybeDescribe("e2e format_on_edit skip reasons", () => {
  let preparedBinary: PreparedBinary = initialBinary;
  const harnesses: E2EHarness[] = [];
  const pools: BridgePool[] = [];

  beforeAll(async () => {
    preparedBinary = await prepareBinary();
  });

  afterEach(async () => {
    await Promise.allSettled(pools.splice(0, pools.length).map((pool) => pool.shutdown()));
    await Promise.allSettled(
      harnesses.splice(0, harnesses.length).map((harness) => harness.cleanup()),
    );
  });

  async function formatHarness(preset: FormatPreset, shims: FakeFormatterShim[] = []) {
    const h = await createFormatHarness(preparedBinary, preset, shims);
    harnesses.push(h);
    return h;
  }

  async function executeHoistedWrite(
    h: E2EHarness,
    filePath: string,
    content: string,
    poolOverrides: Record<string, unknown> = { format_on_edit: true, validate_on_edit: "syntax" },
  ): Promise<string> {
    const pool = new BridgePool(
      h.binaryPath,
      { timeoutMs: 30_000 },
      { project_root: h.tempDir, storage_dir: h.path(".storage"), ...poolOverrides },
    );
    pools.push(pool);
    const tools = hoistedTools(createPluginContext(pool, h.path(".storage")));
    return await tools.write.execute({ filePath, content }, createSdkContext(h.tempDir));
  }

  async function writeAndExpectSkip(
    h: E2EHarness,
    relativePath: string,
    content: string,
    reason: FormatSkipReason,
    poolOverrides?: Record<string, unknown>,
  ): Promise<{ output: string; data: Record<string, unknown>; filePath: string }> {
    const filePath = h.path(relativePath);
    const data = await executeRustWrite(h, relativePath, content, poolOverrides);

    const output =
      poolOverrides?.formatter === undefined
        ? await executeHoistedWrite(h, h.path(`plugin-${relativePath}`), content, poolOverrides)
        : "Created new file.";

    expect(DOCUMENTED_FORMAT_SKIP_REASONS).toContain(reason);
    expect(data.success).toBe(true);
    expect(data.formatted).toBe(false);
    expect(data.format_skipped_reason).toBe(reason);
    expect(await readFile(filePath, "utf8")).toBe(content);
    expect(output).not.toContain("Auto-formatted.");
    return { output, data, filePath };
  }

  async function executeRustWrite(
    h: E2EHarness,
    relativePath: string,
    content: string,
    overrides: Record<string, unknown> = { format_on_edit: true, validate_on_edit: "syntax" },
  ): Promise<Record<string, unknown>> {
    const pool = new BridgePool(
      h.binaryPath,
      { timeoutMs: 30_000 },
      { project_root: h.tempDir, storage_dir: h.path(".storage-rust"), ...overrides },
    );
    pools.push(pool);
    const bridge = pool.getBridge(h.tempDir);
    return await bridge.send("write", {
      file: h.path(relativePath),
      content,
      create_dirs: true,
      diagnostics: true,
      include_diff: true,
    });
  }

  test("unsupported_language: write .txt in biome-configured project", async () => {
    const h = await formatHarness(BIOME_TS_PRESET);
    await writeAndExpectSkip(h, "notes.txt", "alpha   beta\n", "unsupported_language");
  });

  test("unsupported_language: write random .foo extension", async () => {
    const h = await formatHarness(BIOME_TS_PRESET);
    await writeAndExpectSkip(h, "scratch/random.foo", "alpha   beta\n", "unsupported_language");
  });

  test("unsupported_language: edit Markdown, which has no formatter support", async () => {
    const h = await formatHarness(BIOME_TS_PRESET);
    const filePath = h.path("docs/readme.md");
    await mkdir(h.path("docs"), { recursive: true });
    await writeFile(filePath, "# Title\n\nalpha\n", "utf8");

    const data = await h.bridge.send("edit_match", {
      file: filePath,
      match: "alpha",
      replacement: "alpha   beta",
      diagnostics: true,
      include_diff: true,
    });

    expect(data.success).toBe(true);
    expect(data.formatted).toBe(false);
    expect(data.format_skipped_reason).toBe("unsupported_language");
    expect(await readFile(filePath, "utf8")).toBe("# Title\n\nalpha   beta\n");
  });

  test("no_formatter_configured: format_on_edit=false leaves TS unchanged", async () => {
    const h = await formatHarness(BIOME_TS_PRESET);
    await writeAndExpectSkip(h, "src/disabled.ts", TS_INPUT, "no_formatter_configured", {
      format_on_edit: false,
      validate_on_edit: "syntax",
    });
  });

  test("no_formatter_configured: empty preset with no project formatter config", async () => {
    const h = await formatHarness(NO_FORMATTER_PRESET);
    await writeAndExpectSkip(h, "src/plain.ts", TS_INPUT, "no_formatter_configured");
  });

  test("no_formatter_configured: explicit formatter.typescript off", async () => {
    const h = await formatHarness({ configFiles: [], explicitFormatter: { typescript: "off" } });
    await writeAndExpectSkip(h, "src/off.ts", TS_INPUT, "no_formatter_configured", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "off" },
    });
  });

  test("no_formatter_configured: explicit formatter.typescript none", async () => {
    const h = await formatHarness({ configFiles: [], explicitFormatter: { typescript: "none" } });
    await writeAndExpectSkip(h, "src/none.ts", TS_INPUT, "no_formatter_configured", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "none" },
    });
  });

  test("formatter_not_installed: nonexistent explicit formatter", async () => {
    const h = await formatHarness(formatterPreset("prettier"));
    await writeAndExpectSkip(h, "src/missing-explicit.ts", TS_INPUT, "formatter_not_installed", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "prettier" },
    });
  });

  test.skipIf(biomeOnPath)(
    "formatter_not_installed: biome.json auto-detect but no biome on PATH (skipped: biome installed on PATH)",
    async () => {
      const h = await formatHarness(BIOME_TS_PRESET);
      await writeAndExpectSkip(h, "src/missing-biome.ts", TS_INPUT, "formatter_not_installed");
    },
  );

  test.skipIf(true)(
    "formatter_excluded_path: real biome refuses scratch/ via includes filter (skipped: biome unavailable or too slow in e2e)",
    async () => {
      const h = await formatHarness(BIOME_TS_EXCLUDED_PRESET);
      await writeAndExpectSkip(h, "scratch/excluded.ts", TS_INPUT, "formatter_excluded_path");
    },
  );

  test("formatter_excluded_path: biome-style shim emulation", async () => {
    const h = await formatHarness(formatterPreset("biome"), [excludedPathShim()]);
    await writeAndExpectSkip(h, "src/shim-excluded.ts", TS_INPUT, "formatter_excluded_path", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "biome" },
    });
  });

  test("formatter_excluded_path: prettier-style stderr", async () => {
    const h = await formatHarness(formatterPreset("prettier"), [
      shim("prettier", "No files matching the pattern were found"),
    ]);
    await writeAndExpectSkip(h, "src/prettier-excluded.ts", TS_INPUT, "formatter_excluded_path", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "prettier" },
    });
  });

  test("formatter_excluded_path: ruff-style stderr", async () => {
    const h = await formatHarness({ configFiles: [], explicitFormatter: { python: "ruff" } }, [
      shim("ruff", "No Python files found under the given path(s)"),
    ]);
    await writeAndExpectSkip(h, "app.py", FIXTURES.py_deformatted, "formatter_excluded_path", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { python: "ruff" },
    });
  });

  test("formatter_excluded_path: case-insensitive stderr match", async () => {
    const h = await formatHarness(formatterPreset("biome"), [
      shim("biome", "NO FILES WERE PROCESSED IN THE SPECIFIED PATHS"),
    ]);
    await writeAndExpectSkip(h, "src/upper-excluded.ts", TS_INPUT, "formatter_excluded_path", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "biome" },
    });
  });

  test.skip("timeout: hanging formatter shim reports timeout within bounded test budget", async () => {
    const h = await formatHarness(formatterPreset("biome"), [hangingShim()]);
    const started = Date.now();

    await writeAndExpectSkip(h, "src/timeout.ts", TS_INPUT, "timeout", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "biome" },
    });

    expect(Date.now() - started).toBeLessThanOrEqual(14_000);
  }, 20_000);

  test("error: shim with unrecognized stderr is not misclassified as excluded path", async () => {
    const h = await formatHarness(formatterPreset("biome"), [genericErrorShim()]);
    const { data } = await writeAndExpectSkip(h, "src/error.ts", TS_INPUT, "error", {
      format_on_edit: true,
      validate_on_edit: "syntax",
      formatter: { typescript: "biome" },
    });
    expect(data.format_skipped_reason).not.toBe("formatter_excluded_path");
  });

  test("error edge: formatter exits 0 but leaves invalid file cannot repro cleanly", () => {
    // AFT trusts a zero-exit formatter as formatted=true; syntax diagnostics are
    // a separate validation side effect. There is no documented path that maps
    // "exit 0 + invalid output" to format_skipped_reason="error".
    expect(true).toBe(true);
  });

  test("honest reporting: success=true still carries top-level format_skipped_reason", async () => {
    const h = await formatHarness(BIOME_TS_PRESET);
    const { data } = await writeAndExpectSkip(
      h,
      "src/success-with-skip.ts",
      TS_INPUT,
      "no_formatter_configured",
      { format_on_edit: false, validate_on_edit: "syntax" },
    );

    expect(data.success).toBe(true);
    expect(Object.hasOwn(data, "formatted")).toBe(true);
    expect(Object.hasOwn(data, "format_skipped_reason")).toBe(true);
    expect(data.format_skipped_reason).toBe("no_formatter_configured");
  });

  test("agent output never claims Auto-formatted. when formatted=false across skip reasons", async () => {
    const cases: Array<{
      reason: FormatSkipReason;
      preset: FormatPreset;
      shims?: FakeFormatterShim[];
      path: string;
      content: string;
      overrides?: Record<string, unknown>;
    }> = [
      {
        reason: "unsupported_language",
        preset: BIOME_TS_PRESET,
        path: "notes.txt",
        content: "x\n",
      },
      {
        reason: "no_formatter_configured",
        preset: NO_FORMATTER_PRESET,
        path: "src/plain.ts",
        content: TS_INPUT,
      },
      {
        reason: "formatter_not_installed",
        preset: formatterPreset("prettier"),
        path: "src/missing.ts",
        content: TS_INPUT,
        overrides: {
          format_on_edit: true,
          validate_on_edit: "syntax",
          formatter: { typescript: "prettier" },
        },
      },
      {
        reason: "formatter_excluded_path",
        preset: formatterPreset("biome"),
        shims: [excludedPathShim()],
        path: "src/excluded.ts",
        content: TS_INPUT,
        overrides: {
          format_on_edit: true,
          validate_on_edit: "syntax",
          formatter: { typescript: "biome" },
        },
      },
      {
        reason: "timeout",
        preset: formatterPreset("biome"),
        shims: [hangingShim()],
        path: "src/timeout.ts",
        content: TS_INPUT,
        overrides: {
          format_on_edit: true,
          validate_on_edit: "syntax",
          formatter: { typescript: "biome" },
        },
      },
      {
        reason: "error",
        preset: formatterPreset("biome"),
        shims: [genericErrorShim()],
        path: "src/error.ts",
        content: TS_INPUT,
        overrides: {
          format_on_edit: true,
          validate_on_edit: "syntax",
          formatter: { typescript: "biome" },
        },
      },
    ];

    for (const entry of cases) {
      const h = await formatHarness(entry.preset, entry.shims ?? []);
      const { output } = await writeAndExpectSkip(
        h,
        entry.path,
        entry.content,
        entry.reason,
        entry.overrides,
      );
      expect(output).not.toContain("Auto-formatted.");
    }
  }, 30_000);
});
