/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));
const tempRoots = new Set<string>();

function createConfigFixture() {
  const root = mkdtempSync(join(tmpdir(), "aft-pi-config-tests-"));
  tempRoots.add(root);

  const home = join(root, "home");
  const userConfigDir = join(home, ".pi", "agent");
  const projectDirectory = join(root, "project");
  const projectConfigDir = join(projectDirectory, ".pi");

  mkdirSync(userConfigDir, { recursive: true });
  mkdirSync(projectConfigDir, { recursive: true });

  return {
    root,
    home,
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
  test("loads user object-map lsp servers with entry defaults", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify(
        {
          lsp: {
            servers: {
              tinymist: {
                extensions: [".typ"],
                binary: "tinymist",
              },
            },
          },
        },
        null,
        2,
      ),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    expect(JSON.parse(result.stdout)).toEqual({
      lsp: {
        servers: {
          tinymist: {
            extensions: [".typ"],
            binary: "tinymist",
            args: [],
            root_markers: [".git"],
            disabled: false,
          },
        },
      },
    });
  });

  test("rejects malformed lsp servers but keeps other config sections", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify(
        {
          format_on_edit: false,
          lsp: {
            servers: {
              tinymist: {
                extensions: [".typ"],
              },
            },
          },
        },
        null,
        2,
      ),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout) as Record<string, unknown>;
    expect(config.format_on_edit).toBe(false);
    expect(config.lsp).toBeUndefined();
    expect(result.stderr).toContain("Partial config loaded — invalid sections skipped");
  });

  test("merges safe lsp fields while stripping project lsp servers", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            tinymist: { extensions: [".typ"], binary: "tinymist" },
          },
          disabled: ["pyright"],
        },
      }),
    );
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            bashls: { extensions: ["sh"], binary: "bash-language-server" },
          },
          disabled: ["yamlls"],
          python: "ty",
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(Object.keys(config.lsp.servers).sort()).toEqual(["tinymist"]);
    // Audit v0.17 #5: project lsp.disabled is stripped — only user-level disabled survives.
    expect(config.lsp.disabled).toEqual(["pyright"]);
    expect(config.lsp.python).toBe("ty");
    expect(result.stderr).toContain(
      `Ignoring lsp.servers, lsp.disabled from project config ${fixture.projectConfigPath}`,
    );
  });

  test("strips project lsp.servers while preserving user lsp.servers", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            tinymist: { extensions: [".typ"], binary: "tinymist" },
          },
        },
      }),
    );
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            evil: { extensions: [".evil"], binary: "./node_modules/.bin/evil-lsp" },
          },
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(Object.keys(config.lsp.servers)).toEqual(["tinymist"]);
    expect(config.lsp.servers.tinymist.binary).toBe("tinymist");
    expect(config.lsp.servers.evil).toBeUndefined();
    expect(result.stderr).toContain(
      `Ignoring lsp.servers from project config ${fixture.projectConfigPath}`,
    );
  });

  test("strips project lsp.versions", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          versions: { "typescript-language-server": "999.0.0" },
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(config.lsp).toBeUndefined();
    expect(result.stderr).toContain(
      `Ignoring lsp.versions from project config ${fixture.projectConfigPath}`,
    );
  });

  test("strips project lsp.auto_install", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          auto_install: false,
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(config.lsp).toBeUndefined();
    expect(result.stderr).toContain(
      `Ignoring lsp.auto_install from project config ${fixture.projectConfigPath}`,
    );
  });

  test("strips project lsp.grace_days", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          // Audit-2 v0.17 #10: grace_days schema is .positive() now; use 1 to
          // exercise strip behavior with a schema-valid security-relevant value.
          grace_days: 1,
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(config.lsp).toBeUndefined();
    expect(result.stderr).toContain(
      `Ignoring lsp.grace_days from project config ${fixture.projectConfigPath}`,
    );
  });

  // Audit v0.17 #5: project lsp.disabled is now stripped (user-only).
  test("strips project lsp.disabled", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          disabled: ["pyright", "yamlls"],
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(config.lsp).toBeUndefined();
    expect(result.stderr).toContain(
      `Ignoring lsp.disabled from project config ${fixture.projectConfigPath}`,
    );
  });

  test("preserves project lsp.python", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          python: "ty",
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(config.lsp.python).toBe("ty");
    expect(result.stderr).not.toContain("these LSP settings only honor user-level config");
  });

  test("keeps user executable-origin lsp settings when project also sets every lsp key", () => {
    const fixture = createConfigFixture();
    writeFileSync(
      fixture.userConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            tinymist: { extensions: [".typ"], binary: "tinymist" },
          },
          versions: { "typescript-language-server": "4.4.0" },
          auto_install: false,
          grace_days: 14,
          disabled: ["pyright"],
          python: "pyright",
        },
      }),
    );
    writeFileSync(
      fixture.projectConfigPath,
      JSON.stringify({
        lsp: {
          servers: {
            evil: { extensions: [".evil"], binary: "./node_modules/.bin/evil-lsp" },
          },
          versions: {
            "typescript-language-server": "999.0.0",
            "evil/package": "1.0.0",
          },
          auto_install: true,
          // Audit-2 v0.17 #10: schema is .positive() now; use 1 to pass schema
          // validation, then verify strict allowlist still drops it.
          grace_days: 1,
          disabled: ["yamlls"],
          python: "ty",
        },
      }),
    );

    const result = runConfigLoader(fixture.projectDirectory, {
      HOME: fixture.home,
    });

    const config = JSON.parse(result.stdout);
    expect(Object.keys(config.lsp.servers)).toEqual(["tinymist"]);
    expect(config.lsp.versions).toEqual({ "typescript-language-server": "4.4.0" });
    expect(config.lsp.auto_install).toBe(false);
    expect(config.lsp.grace_days).toBe(14);
    // Audit v0.17 #5: only user-level disabled survives — project's ["yamlls"] is stripped.
    expect(config.lsp.disabled).toEqual(["pyright"]);
    expect(config.lsp.python).toBe("ty");
    expect(result.stderr).toContain(
      `Ignoring lsp.servers, lsp.versions, lsp.auto_install, lsp.grace_days, lsp.disabled from project config ${fixture.projectConfigPath}`,
    );
  });
});
