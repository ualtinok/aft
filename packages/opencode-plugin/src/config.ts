import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { type ParseError, parse, printParseErrorCode } from "jsonc-parser";
import { z } from "zod";

// ---------------------------------------------------------------------------
// Zod schema
// ---------------------------------------------------------------------------

const FormatterEnum = z.enum([
  "biome",
  "prettier",
  "deno",
  "ruff",
  "black",
  "rustfmt",
  "goimports",
  "gofmt",
  "none",
]);

const CheckerEnum = z.enum([
  "tsc",
  "biome",
  "pyright",
  "ruff",
  "cargo",
  "go",
  "staticcheck",
  "none",
]);

export const AftConfigSchema = z.object({
  /** Whether to auto-format files after edits. Default: true. */
  format_on_edit: z.boolean().optional(),
  /** Auto-validate after edits: "syntax" (tree-sitter) or "full" (runs type checker). */
  validate_on_edit: z.enum(["syntax", "full"]).optional(),
  /** Per-language formatter overrides. Keys: "typescript", "python", "rust", "go". */
  formatter: z.record(z.string(), FormatterEnum).optional(),
  /** Per-language type checker overrides. Keys: "typescript", "python", "rust", "go". */
  checker: z.record(z.string(), CheckerEnum).optional(),
  /**
   * Replace opencode's built-in read/write/edit/apply_patch tools with AFT's
   * faster Rust implementations. Adds backup tracking, auto-formatting,
   * inline diagnostics, and permission checks. Default: true.
   */
  hoist_builtin_tools: z.boolean().optional(),
});

export type AftConfig = z.infer<typeof AftConfigSchema>;

// ---------------------------------------------------------------------------
// JSONC parsing (same approach as oh-my-opencode)
// ---------------------------------------------------------------------------

function parseJsonc<T = unknown>(content: string): T {
  const errors: ParseError[] = [];
  const result = parse(content, errors, {
    allowTrailingComma: true,
    disallowComments: false,
  }) as T;

  if (errors.length > 0) {
    const errorMessages = errors
      .map((e) => `${printParseErrorCode(e.error)} at offset ${e.offset}`)
      .join(", ");
    throw new SyntaxError(`JSONC parse error: ${errorMessages}`);
  }

  return result;
}

// ---------------------------------------------------------------------------
// Config file detection (.jsonc preferred over .json)
// ---------------------------------------------------------------------------

function detectConfigFile(basePath: string): {
  format: "json" | "jsonc" | "none";
  path: string;
} {
  const jsoncPath = `${basePath}.jsonc`;
  const jsonPath = `${basePath}.json`;

  if (existsSync(jsoncPath)) {
    return { format: "jsonc", path: jsoncPath };
  }
  if (existsSync(jsonPath)) {
    return { format: "json", path: jsonPath };
  }
  return { format: "none", path: jsonPath };
}

// ---------------------------------------------------------------------------
// Partial parse (valid sections survive, invalid sections are skipped)
// ---------------------------------------------------------------------------

function parseConfigPartially(rawConfig: Record<string, unknown>): AftConfig | null {
  const fullResult = AftConfigSchema.safeParse(rawConfig);
  if (fullResult.success) {
    return fullResult.data;
  }

  const partialConfig: Record<string, unknown> = {};
  const invalidSections: string[] = [];

  for (const key of Object.keys(rawConfig)) {
    const sectionResult = AftConfigSchema.safeParse({ [key]: rawConfig[key] });
    if (sectionResult.success) {
      const parsed = sectionResult.data as Record<string, unknown>;
      if (parsed[key] !== undefined) {
        partialConfig[key] = parsed[key];
      }
    } else {
      const sectionErrors = sectionResult.error.issues
        .filter((i) => i.path[0] === key)
        .map((i) => `${i.path.join(".")}: ${i.message}`)
        .join(", ");
      if (sectionErrors) {
        invalidSections.push(`${key}: ${sectionErrors}`);
      }
    }
  }

  if (invalidSections.length > 0) {
    console.error(
      `[aft-plugin] Partial config loaded — invalid sections skipped: ${invalidSections.join("; ")}`,
    );
  }

  return partialConfig as AftConfig;
}

// ---------------------------------------------------------------------------
// Load config from a single file path
// ---------------------------------------------------------------------------

function loadConfigFromPath(configPath: string): AftConfig | null {
  try {
    if (!existsSync(configPath)) {
      return null;
    }

    const content = readFileSync(configPath, "utf-8");
    const rawConfig = parseJsonc<Record<string, unknown>>(content);
    const result = AftConfigSchema.safeParse(rawConfig);

    if (result.success) {
      console.error(`[aft-plugin] Config loaded from ${configPath}`);
      return result.data;
    }

    const errorMsg = result.error.issues.map((i) => `${i.path.join(".")}: ${i.message}`).join(", ");
    console.error(`[aft-plugin] Config validation error in ${configPath}: ${errorMsg}`);

    return parseConfigPartially(rawConfig);
  } catch (err) {
    const errorMsg = err instanceof Error ? err.message : String(err);
    console.error(`[aft-plugin] Error loading config from ${configPath}: ${errorMsg}`);
    return null;
  }
}

// ---------------------------------------------------------------------------
// Merge configs (project overrides user, simple shallow merge for flat schema)
// ---------------------------------------------------------------------------

function mergeConfigs(base: AftConfig, override: AftConfig): AftConfig {
  return {
    ...base,
    ...override,
    // Deep-merge language-scoped maps instead of replacing
    formatter: { ...base.formatter, ...override.formatter },
    checker: { ...base.checker, ...override.checker },
  };
}

// ---------------------------------------------------------------------------
// OpenCode config directory detection (same logic as oh-my-opencode)
// ---------------------------------------------------------------------------

function getOpenCodeConfigDir(): string {
  // XDG_CONFIG_HOME or ~/.config, then /opencode
  const xdgConfig = process.env.XDG_CONFIG_HOME || join(process.env.HOME || "~", ".config");
  return join(xdgConfig, "opencode");
}

// ---------------------------------------------------------------------------
// Public API: loadAftConfig
// ---------------------------------------------------------------------------

/**
 * Load AFT config using the same two-level pattern as oh-my-opencode:
 *
 * 1. User-level:    ~/.config/opencode/aft.jsonc (or .json)
 * 2. Project-level: <project>/.opencode/aft.jsonc (or .json)
 *
 * Project config merges on top of user config.
 * Both support JSONC (comments allowed).
 * Invalid sections are skipped, valid sections still load.
 */
export function loadAftConfig(projectDirectory: string): AftConfig {
  // User-level config
  const configDir = getOpenCodeConfigDir();
  const userBasePath = join(configDir, "aft");
  const userDetected = detectConfigFile(userBasePath);
  const userConfigPath =
    userDetected.format !== "none" ? userDetected.path : `${userBasePath}.json`;

  // Project-level config
  const projectBasePath = join(projectDirectory, ".opencode", "aft");
  const projectDetected = detectConfigFile(projectBasePath);
  const projectConfigPath =
    projectDetected.format !== "none" ? projectDetected.path : `${projectBasePath}.json`;

  // Load user config first (base)
  let config: AftConfig = loadConfigFromPath(userConfigPath) ?? {};

  // Override with project config
  const projectConfig = loadConfigFromPath(projectConfigPath);
  if (projectConfig) {
    config = mergeConfigs(config, projectConfig);
  }

  return config;
}
