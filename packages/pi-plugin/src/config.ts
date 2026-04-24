import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { z } from "zod";
import { error, log, warn } from "./logger.js";

// ---------------------------------------------------------------------------
// Config shape (mirrors aft-opencode's schema, simplified for Pi)
// ---------------------------------------------------------------------------

export type Formatter =
  | "biome"
  | "prettier"
  | "deno"
  | "ruff"
  | "black"
  | "rustfmt"
  | "goimports"
  | "gofmt"
  | "none";

export type Checker =
  | "tsc"
  | "biome"
  | "pyright"
  | "ruff"
  | "cargo"
  | "go"
  | "staticcheck"
  | "none";

export type SemanticBackend = "fastembed" | "openai_compatible" | "ollama";

export interface SemanticConfig {
  backend?: SemanticBackend;
  model?: string;
  base_url?: string;
  api_key_env?: string;
  timeout_ms?: number;
  max_batch_size?: number;
}

export type ToolSurface = "minimal" | "recommended" | "all";

export interface AftConfig {
  format_on_edit?: boolean;
  validate_on_edit?: "syntax" | "full";
  formatter?: Record<string, Formatter>;
  checker?: Record<string, Checker>;
  tool_surface?: ToolSurface;
  disabled_tools?: string[];
  restrict_to_project_root?: boolean;
  experimental_search_index?: boolean;
  experimental_semantic_search?: boolean;
  url_fetch_allow_private?: boolean;
  semantic?: SemanticConfig;
  /**
   * Maximum source files allowed for call-graph operations (callers, trace_to,
   * trace_data, impact). Projects above this size return `project_too_large`.
   * Default: 20000 (applied Rust-side; undefined here means "use default").
   */
  max_callgraph_files?: number;
}

// TODO: move this schema to a shared package/module with aft-opencode to avoid drift.

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

const SemanticConfigSchema = z.object({
  backend: z.enum(["fastembed", "openai_compatible", "ollama"]).optional(),
  model: z.string().trim().min(1).optional(),
  base_url: z.string().trim().min(1).optional(),
  api_key_env: z.string().trim().min(1).optional(),
  timeout_ms: z.number().int().positive().optional(),
  max_batch_size: z.number().int().positive().optional(),
});

export const AftConfigSchema = z.object({
  format_on_edit: z.boolean().optional(),
  validate_on_edit: z.enum(["syntax", "full"]).optional(),
  formatter: z.record(z.string(), FormatterEnum).optional(),
  checker: z.record(z.string(), CheckerEnum).optional(),
  tool_surface: z.enum(["minimal", "recommended", "all"]).optional(),
  disabled_tools: z.array(z.string()).optional(),
  restrict_to_project_root: z.boolean().optional(),
  experimental_search_index: z.boolean().optional(),
  experimental_semantic_search: z.boolean().optional(),
  url_fetch_allow_private: z.boolean().optional(),
  semantic: SemanticConfigSchema.optional(),
  max_callgraph_files: z.number().int().positive().optional(),
});

// ---------------------------------------------------------------------------
// Minimal JSONC parser (strips comments + trailing commas before JSON.parse).
// Kept inline to avoid adding comment-json as a runtime dep for Pi.
// ---------------------------------------------------------------------------

function stripJsonc(input: string): string {
  let result = "";
  let i = 0;
  const n = input.length;
  let inString = false;
  let stringChar = "";
  while (i < n) {
    const ch = input[i];
    const next = input[i + 1];
    if (inString) {
      result += ch;
      if (ch === "\\" && i + 1 < n) {
        result += input[i + 1];
        i += 2;
        continue;
      }
      if (ch === stringChar) inString = false;
      i++;
      continue;
    }
    if (ch === '"' || ch === "'") {
      inString = true;
      stringChar = ch;
      result += ch;
      i++;
      continue;
    }
    if (ch === "/" && next === "/") {
      // line comment
      while (i < n && input[i] !== "\n") i++;
      continue;
    }
    if (ch === "/" && next === "*") {
      i += 2;
      while (i < n && !(input[i] === "*" && input[i + 1] === "/")) i++;
      i += 2;
      continue;
    }
    result += ch;
    i++;
  }
  // Strip trailing commas before } or ]
  return result.replace(/,(\s*[}\]])/g, "$1");
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
  if (existsSync(jsoncPath)) return { format: "jsonc", path: jsoncPath };
  if (existsSync(jsonPath)) return { format: "json", path: jsonPath };
  return { format: "none", path: jsonPath };
}

function loadConfigFromPath(configPath: string): AftConfig | null {
  try {
    if (!existsSync(configPath)) return null;
    const content = readFileSync(configPath, "utf-8");
    const parsed = JSON.parse(stripJsonc(content)) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      warn(`Config validation error in ${configPath}: root must be an object`);
      return null;
    }
    const rawConfig = parsed as Record<string, unknown>;
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

function parseConfigPartially(rawConfig: Record<string, unknown>): AftConfig {
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
// Merge configs (project overrides user, deep-merge nested maps)
// ---------------------------------------------------------------------------

function mergeSemanticConfig(
  base?: SemanticConfig,
  override?: SemanticConfig,
): SemanticConfig | undefined {
  // SECURITY: Only safe fields from project override are merged.
  // Sensitive fields (backend, base_url, api_key_env) must come from user config.
  const projectSafe: SemanticConfig = {};
  if (override?.model !== undefined) projectSafe.model = override.model;
  if (override?.timeout_ms !== undefined) projectSafe.timeout_ms = override.timeout_ms;
  if (override?.max_batch_size !== undefined) projectSafe.max_batch_size = override.max_batch_size;

  const semantic: SemanticConfig = { ...base, ...projectSafe };
  if (Object.values(semantic).every((v) => v === undefined)) return undefined;

  return Object.fromEntries(
    Object.entries(semantic).filter(([, v]) => v !== undefined),
  ) as SemanticConfig;
}

function mergeConfigs(base: AftConfig, override: AftConfig): AftConfig {
  const disabledTools = [...(base.disabled_tools ?? []), ...(override.disabled_tools ?? [])];
  const formatter = { ...base.formatter, ...override.formatter };
  const checker = { ...base.checker, ...override.checker };
  const semantic = mergeSemanticConfig(base.semantic, override.semantic);

  // SECURITY: Strip sensitive semantic fields from override before spreading.
  const { semantic: _stripSemantic, ...safeOverride } = override;

  return {
    ...base,
    ...safeOverride,
    ...(Object.keys(formatter).length > 0 ? { formatter } : {}),
    ...(Object.keys(checker).length > 0 ? { checker } : {}),
    semantic,
    ...(disabledTools.length > 0 ? { disabled_tools: [...new Set(disabledTools)] } : {}),
  };
}

// ---------------------------------------------------------------------------
// Pi config directory detection
//
// Pi's convention:
//   - Global: ~/.pi/agent/
//   - Project: <projectDir>/.pi/
// ---------------------------------------------------------------------------

function getGlobalPiDir(): string {
  return join(homedir(), ".pi", "agent");
}

/**
 * Load AFT config:
 *   1. User-level:    ~/.pi/agent/aft.jsonc (or .json)
 *   2. Project-level: <project>/.pi/aft.jsonc (or .json)
 *
 * Project config merges on top of user config.
 */
export function loadAftConfig(projectDirectory: string): AftConfig {
  const userBasePath = join(getGlobalPiDir(), "aft");
  const userDetected = detectConfigFile(userBasePath);
  const userConfigPath =
    userDetected.format !== "none" ? userDetected.path : `${userBasePath}.json`;

  const projectBasePath = join(projectDirectory, ".pi", "aft");
  const projectDetected = detectConfigFile(projectBasePath);
  const projectConfigPath =
    projectDetected.format !== "none" ? projectDetected.path : `${projectBasePath}.json`;

  let config: AftConfig = loadConfigFromPath(userConfigPath) ?? {};

  const projectConfig = loadConfigFromPath(projectConfigPath);
  if (projectConfig) {
    if (
      projectConfig.semantic?.backend !== undefined ||
      projectConfig.semantic?.base_url !== undefined ||
      projectConfig.semantic?.api_key_env !== undefined
    ) {
      warn(
        "Ignoring semantic.backend/base_url/api_key_env from project config (security: use user config for external backends)",
      );
    }
    config = mergeConfigs(config, projectConfig);
  }

  return config;
}
