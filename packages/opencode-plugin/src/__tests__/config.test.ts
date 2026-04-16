import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));
const tempRoots = new Set<string>();

function createConfigFixture() {
  const root = mkdtempSync(join(tmpdir(), "aft-config-tests-"));
  tempRoots.add(root);

  const xdgConfigHome = join(root, "xdg-config");
  const userConfigDir = join(xdgConfigHome, "opencode");
  const projectDirectory = join(root, "project");
  const projectConfigDir = join(projectDirectory, ".opencode");

  mkdirSync(userConfigDir, { recursive: true });
  mkdirSync(projectConfigDir, { recursive: true });

  return {
    root,
    xdgConfigHome,
    projectDirectory,
    userConfigPath: join(userConfigDir, "aft.jsonc"),
    projectConfigPath: join(projectConfigDir, "aft.jsonc"),
  };
}

function runConfigLoader(projectDirectory: string, env: Record<string, string>) {
  const script = `
    import { loadAftConfig } from "./src/config.ts";
    console.log(JSON.stringify(loadAftConfig(process.env.PROJECT_DIR!)));
  `;
  const result = spawnSync(process.execPath, ["-e", script], {
    cwd: packageRoot,
    env: { ...process.env, AFT_LOG_STDERR: "1", ...env, PROJECT_DIR: projectDirectory },
    encoding: "utf8",
  });

  expect(result.error).toBeUndefined();
  expect(result.status).toBe(0);

  return {
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
  };
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("loadAftConfig", () => {
  test("returns an empty config when user and project config files are missing", () => {
    const fixture = createConfigFixture();
    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: join(fixture.root, "home"),
      XDG_CONFIG_HOME: fixture.xdgConfigHome,
    });

    expect(JSON.parse(result.stdout)).toEqual({});
    expect(result.stderr).toBe("");
  });

  test("logs and skips malformed JSONC", () => {
    const fixture = createConfigFixture();
    writeFileSync(fixture.projectConfigPath, "{ invalid jsonc");

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: join(fixture.root, "home"),
      XDG_CONFIG_HOME: fixture.xdgConfigHome,
    });

    expect(JSON.parse(result.stdout)).toEqual({});
    expect(result.stderr).toContain(
      `[aft-plugin] Error loading config from ${fixture.projectConfigPath}:`,
    );
    expect(result.stderr).toContain("is not valid JSON");
  });

  test("keeps valid sections when invalid config values are present", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        format_on_edit: "yes please",
        hoist_builtin_tools: false,
        formatter: { typescript: "biome" },
        checker: { typescript: 123 },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: join(fixture.root, "home"),
      XDG_CONFIG_HOME: fixture.xdgConfigHome,
    });

    expect(JSON.parse(result.stdout)).toEqual({
      hoist_builtin_tools: false,
      formatter: { typescript: "biome" },
    });
    expect(result.stderr).toContain("Config validation error in");
    expect(result.stderr).toContain("Partial config loaded — invalid sections skipped");
  });

  test("deep merges project config on top of user config", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        format_on_edit: false,
        formatter: { typescript: "biome", python: "black" },
        checker: { python: "ruff" },
      }),
    );
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        validate_on_edit: "full",
        hoist_builtin_tools: true,
        formatter: { typescript: "prettier" },
        checker: { typescript: "tsc" },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: join(fixture.root, "home"),
      XDG_CONFIG_HOME: fixture.xdgConfigHome,
    });

    expect(JSON.parse(result.stdout)).toEqual({
      format_on_edit: false,
      validate_on_edit: "full",
      hoist_builtin_tools: true,
      formatter: { typescript: "prettier", python: "black" },
      checker: { python: "ruff", typescript: "tsc" },
    });
    expect(result.stderr).toContain(`Config loaded from ${fixture.userConfigPath}`);
    expect(result.stderr).toContain(`Config loaded from ${fixture.projectConfigPath}`);
  });

  test("loads semantic config block and propagates nested fields", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        semantic: {
          backend: "openai_compatible",
          model: "text-embedding-3-small",
          base_url: "https://api.example.test/v1",
          api_key_env: "AFT_SEMANTIC_API_KEY",
          timeout_ms: 15_000,
          max_batch_size: 32,
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: join(fixture.root, "home"),
      XDG_CONFIG_HOME: fixture.xdgConfigHome,
    });

    expect(JSON.parse(result.stdout)).toEqual({
      semantic: {
        backend: "openai_compatible",
        model: "text-embedding-3-small",
        base_url: "https://api.example.test/v1",
        api_key_env: "AFT_SEMANTIC_API_KEY",
        timeout_ms: 15000,
        max_batch_size: 32,
      },
    });
    expect(result.stderr).toContain(`Config loaded from ${fixture.userConfigPath}`);
  });

  test("rejects invalid semantic backend value as malformed section", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        semantic: {
          backend: "gpt-4",
          timeout_ms: 1000,
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: join(fixture.root, "home"),
      XDG_CONFIG_HOME: fixture.xdgConfigHome,
    });

    expect(JSON.parse(result.stdout)).toEqual({});
    expect(result.stderr).toContain("Partial config loaded — invalid sections skipped");
  });
});
