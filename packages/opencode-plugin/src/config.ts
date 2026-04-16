import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { parse as parseJsonc } from "comment-json";
import { z } from "zod";
import { error, log, warn } from "./logger.js";

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

const SemanticBackendEnum = z.enum(["fastembed", "openai_compatible", "ollama"]);

const SemanticConfigSchema = z.object({
  /** Semantic backend type: local fastembed, OpenAI-compatible API, or Ollama. */
  backend: SemanticBackendEnum.optional(),
  /** Model identifier passed to the selected semantic backend. */
  model: z.string().trim().min(1).optional(),
  /** Base URL of the backend API endpoint. */
  base_url: z.string().trim().min(1).optional(),
  /** Environment variable that contains the API key used by external backends. */
  api_key_env: z.string().trim().min(1).optional(),
  /** Backend request timeout in milliseconds. */
  timeout_ms: z.number().int().positive().optional(),
  /** Maximum batch size used by the semantic pipeline. */
  max_batch_size: z.number().int().positive().optional(),
});

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
  /**
   * Tool surface level. Controls which tools are registered:
   * - "minimal":     aft_outline, aft_zoom, aft_safety (no hoisting)
   * - "recommended": minimal + hoisted read/write/edit/apply_patch + lsp_diagnostics
   *                  + ast_grep_search/replace + aft_import (default)
   * - "all":         recommended + aft_navigate, aft_delete, aft_move, aft_transform, aft_refactor
   */
  tool_surface: z.enum(["minimal", "recommended", "all"]).optional(),
  /**
   * List of tool names to disable. Disabled tools are not registered with
   * OpenCode and will be invisible to agents. Use exact tool names, e.g.
   * ["aft_navigate", "aft_refactor"]. Hoisted names ("read", "edit") and
   * aft-prefixed names both work. Applied after tool_surface filtering.
   */
  disabled_tools: z.array(z.string()).optional(),
  /**
   * Restrict file operations to within the project root directory.
   * When true, write-capable commands reject paths outside project_root.
   * Default: false (matches OpenCode's built-in behavior).
   */
  restrict_to_project_root: z.boolean().optional(),
  /** Enable experimental indexed search for grep and glob hoisting. Default: false. */
  experimental_search_index: z.boolean().optional(),
  /** Enable experimental semantic search. Default: false. */
  experimental_semantic_search: z.boolean().optional(),
  /** External semantic backend configuration for embedding and retrieval. */
  semantic: SemanticConfigSchema.optional(),
});

export type AftConfig = z.infer<typeof AftConfigSchema>;

// JSONC parsing via comment-json (bundled at build time).
// Preserves comments during round-trip in tui-config.ts.

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
    warn(`Partial config loaded — invalid sections skipped: ${invalidSections.join("; ")}`);
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
      log(`Config loaded from ${configPath}`);
      return result.data;
    }

    const errorMsg = result.error.issues.map((i) => `${i.path.join(".")}: ${i.message}`).join(", ");
    warn(`Config validation error in ${configPath}: ${errorMsg}`);

    return parseConfigPartially(rawConfig);
  } catch (err) {
    const errorMsg = err instanceof Error ? err.message : String(err);
    error(`Error loading config from ${configPath}: ${errorMsg}`);
    return null;
  }
}

// ---------------------------------------------------------------------------
// Merge configs (project overrides user, simple shallow merge for flat schema)
// ---------------------------------------------------------------------------

function mergeConfigs(base: AftConfig, override: AftConfig): AftConfig {
  // Union disabled_tools from both levels (user + project)
  const disabledTools = [...(base.disabled_tools ?? []), ...(override.disabled_tools ?? [])];

  return {
    ...base,
    ...override,
    // Deep-merge language-scoped maps instead of replacing
    formatter: { ...base.formatter, ...override.formatter },
    checker: { ...base.checker, ...override.checker },
    // Union — both levels contribute to the disabled set
    ...(disabledTools.length > 0 ? { disabled_tools: [...new Set(disabledTools)] } : {}),
  };
}

// ---------------------------------------------------------------------------
// OpenCode config directory detection (same logic as oh-my-opencode)
// ---------------------------------------------------------------------------

function getOpenCodeConfigDir(): string {
  const envDir = process.env.OPENCODE_CONFIG_DIR?.trim();
  if (envDir) {
    return envDir;
  }

  // XDG_CONFIG_HOME or homedir()/.config, then /opencode
  const xdgConfig = process.env.XDG_CONFIG_HOME || join(homedir(), ".config");
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
