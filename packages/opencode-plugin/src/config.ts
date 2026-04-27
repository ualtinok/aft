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

const LspExtensionSchema = z
  .string()
  .trim()
  .min(1)
  .refine((value) => value.replace(/^\.+/, "").length > 0, {
    message: "Extension must include characters other than leading dots",
  });

const LspServerEntrySchema = z.object({
  extensions: z.array(LspExtensionSchema).min(1),
  binary: z.string().trim().min(1),
  args: z.array(z.string()).optional().default([]),
  root_markers: z.array(z.string().trim().min(1)).optional().default([".git"]),
  disabled: z.boolean().optional().default(false),
  /** Extra environment variables passed to the LSP server child process. */
  env: z.record(z.string().min(1), z.string()).optional(),
  /** JSON value passed as `initializationOptions` in the LSP `initialize` request. */
  initialization_options: z.unknown().optional(),
});

export const LspServerSchema = LspServerEntrySchema.extend({
  id: z.string().trim().min(1),
});

const LspConfigSchema = z.object({
  servers: z.record(z.string().trim().min(1), LspServerEntrySchema).optional(),
  disabled: z.array(z.string().trim().min(1)).optional(),
  python: z.enum(["pyright", "ty", "auto"]).optional(),
  /**
   * Auto-install npm-distributed and GitHub-release language servers when
   * the project needs them. Default: true. Set false to require manual
   * install via PATH.
   */
  auto_install: z.boolean().optional(),
  /**
   * Supply-chain grace window. AFT only installs versions that have been
   * on the registry / GitHub releases for at least this many days, defending
   * against newly-published malicious versions that get yanked within hours
   * of detection. Default: 7. User pins via `lsp.versions` bypass this.
   */
  // Audit-2 v0.17 #10: grace_days must be >= 1 because grace_days: 0 disables
  // the supply-chain grace window entirely with no warning. Users debugging
  // can still bypass the grace per-package via `lsp.versions` pins, which is
  // a more explicit and auditable opt-out.
  grace_days: z.number().int().positive().optional(),
  /**
   * Per-package version pin map keyed by npm package or GitHub repo. Pins
   * bypass the grace filter and any weekly version recheck. Examples:
   *   { "typescript-language-server": "5.0.0" }
   *   { "clangd/clangd": "21.1.0" }
   */
  versions: z.record(z.string().trim().min(1), z.string().trim().min(1)).optional(),
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
  /** Enable experimental ty LSP support. Default: false. */
  experimental_lsp_ty: z.boolean().optional(),
  /** User-defined and built-in LSP server configuration. */
  lsp: LspConfigSchema.optional(),
  /** Allow URL fetch tools to request private/link-local hosts. Default: false. */
  url_fetch_allow_private: z.boolean().optional(),
  /** External semantic backend configuration for embedding and retrieval. */
  semantic: SemanticConfigSchema.optional(),
  /**
   * Maximum source files allowed for call-graph operations (callers, trace_to,
   * trace_data, impact). Projects above this size return `project_too_large`
   * instead of attempting the reverse-index build. Does not affect grep,
   * glob, read, edit, or any other tool. Default: 20000.
   */
  max_callgraph_files: z.number().int().positive().optional(),
});

export type AftConfig = z.infer<typeof AftConfigSchema>;

export type LspServerConfig = z.infer<typeof LspServerSchema>;

export interface ConfigureLspServer {
  id: string;
  extensions: string[];
  binary: string;
  args: string[];
  root_markers: string[];
  disabled: boolean;
  env?: Record<string, string>;
  initialization_options?: unknown;
}

export interface ConfigureLspOverrides {
  experimental_lsp_ty?: boolean;
  lsp_servers?: ConfigureLspServer[];
  disabled_lsp?: string[];
}

function normalizeLspExtension(extension: string): string {
  return extension.trim().replace(/^\.+/, "");
}

export function resolveLspConfigForConfigure(config: AftConfig): ConfigureLspOverrides {
  const overrides: ConfigureLspOverrides = {};
  const disabled = new Set(config.lsp?.disabled ?? []);
  let experimentalTy = config.experimental_lsp_ty;

  // Server IDs match Rust's `ServerKind::id_str()` — built-in Pyright is
  // identified as "python", and the experimental Astral checker as "ty".
  // Custom IDs are case-insensitive.
  switch (config.lsp?.python ?? "auto") {
    case "ty":
      experimentalTy = true;
      disabled.add("python");
      break;
    case "pyright":
      experimentalTy = false;
      disabled.add("ty");
      break;
    case "auto":
      break;
  }

  if (experimentalTy !== undefined) {
    overrides.experimental_lsp_ty = experimentalTy;
  }

  const servers = Object.entries(config.lsp?.servers ?? {}).map(([id, server]) => {
    const entry: ConfigureLspServer = {
      id,
      extensions: server.extensions.map(normalizeLspExtension),
      binary: server.binary,
      args: server.args,
      root_markers: server.root_markers,
      disabled: server.disabled,
    };
    if (server.env && Object.keys(server.env).length > 0) {
      entry.env = server.env;
    }
    if (server.initialization_options !== undefined) {
      entry.initialization_options = server.initialization_options;
    }
    return entry;
  });
  if (servers.length > 0) {
    overrides.lsp_servers = servers;
  }

  if (disabled.size > 0) {
    overrides.disabled_lsp = [...disabled];
  }

  return overrides;
}

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
// Merge configs (project overrides user, deep-merge nested maps/blocks)
// ---------------------------------------------------------------------------

function mergeSemanticConfig(
  baseSemantic: AftConfig["semantic"],
  overrideSemantic: AftConfig["semantic"],
): AftConfig["semantic"] {
  // Only include DEFINED safe fields from the project override.
  // Undefined fields must NOT overwrite user-level values via spread.
  const projectSemantic: Record<string, unknown> = {};
  if (overrideSemantic) {
    if (overrideSemantic.model !== undefined) projectSemantic.model = overrideSemantic.model;
    if (overrideSemantic.timeout_ms !== undefined)
      projectSemantic.timeout_ms = overrideSemantic.timeout_ms;
    if (overrideSemantic.max_batch_size !== undefined)
      projectSemantic.max_batch_size = overrideSemantic.max_batch_size;
  }

  const semantic = {
    ...baseSemantic,
    ...projectSemantic,
  };

  if (Object.values(semantic).every((value) => value === undefined)) {
    return undefined;
  }

  return Object.fromEntries(
    Object.entries(semantic).filter(([, value]) => value !== undefined),
  ) as AftConfig["semantic"];
}

function mergeLspConfig(
  baseLsp: AftConfig["lsp"],
  overrideLsp: AftConfig["lsp"],
): AftConfig["lsp"] {
  // STRICT ALLOWLIST: only safe fields from project override are honored.
  //
  // EXECUTABLE-ORIGIN fields (servers, versions, auto_install, grace_days)
  // must come from user config — a hostile repo could otherwise specify
  // which binary AFT installs and runs (audit v0.17 #1).
  //
  // ATTACK-DEFENSE fields (disabled) cannot be set from project config
  // either — a hostile repo could silently disable LSP servers the user
  // relies on, suppressing diagnostics for its own malicious code
  // (audit v0.17 #5).
  //
  // SAFE project-level fields: `python` (per-language preference, no
  //   executable origin) and (none right now).
  const projectLsp: AftConfig["lsp"] = {};
  if (overrideLsp?.python !== undefined) projectLsp.python = overrideLsp.python;

  // disabled comes from user config ONLY.
  const userDisabled = baseLsp?.disabled ?? [];

  const lsp = {
    ...baseLsp,
    ...projectLsp,
    ...(userDisabled.length > 0 ? { disabled: [...userDisabled] } : {}),
  };

  if (Object.values(lsp).every((value) => value === undefined)) {
    return undefined;
  }

  return Object.fromEntries(
    Object.entries(lsp).filter(([, value]) => value !== undefined),
  ) as AftConfig["lsp"];
}

function getProjectLspStrippedKeys(lsp: AftConfig["lsp"]): string[] {
  if (!lsp) {
    return [];
  }

  const strippedKeys: string[] = [];
  if (lsp.servers !== undefined) strippedKeys.push("lsp.servers");
  if (lsp.versions !== undefined) strippedKeys.push("lsp.versions");
  if (lsp.auto_install !== undefined) strippedKeys.push("lsp.auto_install");
  if (lsp.grace_days !== undefined) strippedKeys.push("lsp.grace_days");
  if (lsp.disabled !== undefined) strippedKeys.push("lsp.disabled");
  return strippedKeys;
}

/**
 * Top-level fields that are SAFE to inherit from project config.
 *
 * Anything NOT in this list flows from user config only. This is the
 * strict-allowlist trust boundary — adding a new field requires explicit
 * security review of whether a hostile repo could weaponize it.
 *
 * Audit v0.17 #17: previously `restrict_to_project_root`, `url_fetch_allow_private`,
 * and `max_callgraph_files` flowed through the implicit `...safeOverride` spread,
 * allowing project config to weaken security boundaries.
 *
 * (Note: `storage_dir` is not a config-schema field — the plugin always sets
 * it at configure time. It cannot be set from any aft.jsonc file.)
 */
const PROJECT_SAFE_TOP_LEVEL_FIELDS = new Set<keyof AftConfig>([
  "tool_surface",
  "hoist_builtin_tools",
  "format_on_edit",
  "validate_on_edit",
  "experimental_search_index",
  "experimental_semantic_search",
  "experimental_lsp_ty",
  // "disabled_tools" handled separately — unioned via array merge.
  // "formatter"/"checker" handled separately — deep-merged.
  // "semantic"/"lsp" handled separately — strict field-level merge.
  // "restrict_to_project_root" — USER ONLY (security boundary).
  // "url_fetch_allow_private" — USER ONLY (SSRF surface).
  // "storage_dir" — USER ONLY (controls where AFT writes).
  // "max_callgraph_files" — USER ONLY (resource budget).
]);

function pickProjectSafeFields(override: AftConfig): Partial<AftConfig> {
  const safe: Partial<AftConfig> = {};
  for (const key of PROJECT_SAFE_TOP_LEVEL_FIELDS) {
    if (override[key] !== undefined) {
      // biome-ignore lint/suspicious/noExplicitAny: field-by-field copy with key set guarantee
      (safe as any)[key] = override[key];
    }
  }
  return safe;
}

function getStrippedTopLevelKeys(override: AftConfig): string[] {
  const stripped: string[] = [];
  if (override.restrict_to_project_root !== undefined) stripped.push("restrict_to_project_root");
  if (override.url_fetch_allow_private !== undefined) stripped.push("url_fetch_allow_private");
  if (override.max_callgraph_files !== undefined) stripped.push("max_callgraph_files");
  return stripped;
}

function mergeConfigs(base: AftConfig, override: AftConfig): AftConfig {
  // Union disabled_tools from both levels (user + project).
  // disabled_tools governs WHICH AFT TOOLS the agent sees — a hostile repo
  // disabling tools is a mild annoyance, not a security boundary, so the
  // union is acceptable here.
  const disabledTools = [...(base.disabled_tools ?? []), ...(override.disabled_tools ?? [])];
  const formatter = { ...base.formatter, ...override.formatter };
  const checker = { ...base.checker, ...override.checker };
  const semantic = mergeSemanticConfig(base.semantic, override.semantic);
  const lsp = mergeLspConfig(base.lsp, override.lsp);

  // STRICT ALLOWLIST: only project-safe top-level fields are inherited.
  // See PROJECT_SAFE_TOP_LEVEL_FIELDS above for the full security rationale.
  const safeOverride = pickProjectSafeFields(override);

  return {
    ...base,
    ...safeOverride,
    // Deep-merge language-scoped maps instead of replacing
    ...(Object.keys(formatter).length > 0 ? { formatter } : {}),
    ...(Object.keys(checker).length > 0 ? { checker } : {}),
    ...(lsp ? { lsp } : {}),
    // Always set semantic to the merge result (even if undefined) to prevent
    // override.semantic from leaking through any future spread above.
    semantic,
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
    if (
      projectConfig.semantic?.backend !== undefined ||
      projectConfig.semantic?.base_url !== undefined ||
      projectConfig.semantic?.api_key_env !== undefined
    ) {
      warn(
        "Ignoring semantic.backend/base_url/api_key_env from project config (security: use user config for external backends)",
      );
    }
    const strippedLspKeys = getProjectLspStrippedKeys(projectConfig.lsp);
    if (strippedLspKeys.length > 0) {
      warn(
        `Ignoring ${strippedLspKeys.join(", ")} from project config ${projectConfigPath} (security: these LSP settings only honor user-level config)`,
      );
    }
    const strippedTopLevelKeys = getStrippedTopLevelKeys(projectConfig);
    if (strippedTopLevelKeys.length > 0) {
      warn(
        `Ignoring ${strippedTopLevelKeys.join(", ")} from project config ${projectConfigPath} (security: these settings only honor user-level config — a project should not weaken security boundaries for the user)`,
      );
    }
    config = mergeConfigs(config, projectConfig);
  }

  return config;
}
