/**
 * AFT (Agent File Tools) extension for Pi coding agent.
 *
 * Config is loaded from two levels (project overrides user):
 * - User:    ~/.pi/agent/aft.jsonc (or .json)
 * - Project: <project>/.pi/aft.jsonc (or .json)
 *
 * Tools registered:
 *
 * Hoisting (replace Pi's built-in tools):
 *   - read   → AFT's indexed Rust reader
 *   - write  → AFT's atomic writer with backup + auto-format + LSP diagnostics
 *   - edit   → AFT's fuzzy-match edit with backup + diagnostics
 *   - grep   → AFT's trigram-indexed grep (falls back to ripgrep outside project root)
 *
 * AFT-specific:
 *   - aft_outline    Structural outline (symbols, headings) for files/URLs
 *   - aft_zoom       Symbol-level inspection with call-graph annotations
 *   - aft_search     Semantic search (when experimental_semantic_search=true)
 *   - aft_navigate   Call-graph navigation (callers, call_tree, impact, trace_to, trace_data)
 *   - aft_conflicts  One-call merge conflict inspection
 *   - aft_import     Language-aware import add/remove/organize
 *   - aft_safety     Per-file undo, checkpoints, restore
 *   - aft_delete     Delete file with backup
 *   - aft_move       Move/rename file
 *   - ast_grep_search / ast_grep_replace  AST-aware pattern search/rewrite
 *   - lsp_diagnostics On-demand LSP diagnostics
 *
 * Commands:
 *   - /aft-status    Status dialog (index states, LSP servers, storage dir)
 */

import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { registerStatusCommand } from "./commands/aft-status.js";
import { loadAftConfig, resolveLspConfigForConfigure } from "./config.js";
import { log, warn } from "./logger.js";
import { type ConfigureWarning, deliverConfigureWarnings } from "./notifications.js";
import { ensureOnnxRuntime, getManualInstallHint } from "./onnx-runtime.js";
import { BridgePool } from "./pool.js";
import { findBinary } from "./resolver.js";
import { registerShutdownCleanup } from "./shutdown-hooks.js";
import { registerAstTools } from "./tools/ast.js";
import { registerConflictsTool } from "./tools/conflicts.js";
import { registerFsTools } from "./tools/fs.js";
import { registerHoistedTools } from "./tools/hoisted.js";
import { registerImportTools } from "./tools/imports.js";
import { registerLspTools } from "./tools/lsp.js";
import { registerNavigateTool } from "./tools/navigate.js";
import { registerReadingTools } from "./tools/reading.js";
import { registerRefactorTool } from "./tools/refactor.js";
import { registerSafetyTool } from "./tools/safety.js";
import { registerSemanticTool } from "./tools/semantic.js";
import { registerStructureTool } from "./tools/structure.js";
import type { PluginContext } from "./types.js";

/** Plugin version from package.json. */
const PLUGIN_VERSION: string = (() => {
  try {
    const req = createRequire(import.meta.url);
    return (req("../package.json") as { version: string }).version;
  } catch {
    return "0.0.0";
  }
})();

const ALL_ONLY_TOOLS = new Set([
  "aft_navigate",
  "aft_delete",
  "aft_move",
  "aft_transform",
  "aft_refactor",
]);

function isConfigureWarning(value: unknown): value is ConfigureWarning {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const warning = value as Record<string, unknown>;
  return (
    (warning.kind === "formatter_not_installed" ||
      warning.kind === "checker_not_installed" ||
      warning.kind === "lsp_binary_missing") &&
    typeof warning.hint === "string"
  );
}

function coerceConfigureWarnings(warnings: unknown[]): ConfigureWarning[] {
  return warnings.filter(isConfigureWarning);
}

/** Resolve the AFT storage directory (auth + semantic index + ONNX cache). */
function resolveStorageDir(): string {
  // Pi doesn't expose its data dir via a public API; use ~/.pi/agent/aft as convention.
  return join(homedir(), ".pi", "agent", "aft");
}

/**
 * Tool surface mirrors opencode-plugin: navigate/delete/move/transform/refactor
 * are all-only. recommended exposes hoisted + read/safety/import/ast/lsp/conflicts
 * + experimental search/semantic when enabled.
 *
 * Returns the set of AFT tool names that should be registered given the
 * configured surface + disabled_tools filter. Pi's built-in tools are always
 * present; registering an AFT tool with the same name replaces them.
 */
function resolveToolSurface(config: ReturnType<typeof loadAftConfig>): {
  hoistRead: boolean;
  hoistWrite: boolean;
  hoistEdit: boolean;
  hoistGrep: boolean;
  outline: boolean;
  zoom: boolean;
  semantic: boolean;
  navigate: boolean;
  conflicts: boolean;
  importTool: boolean;
  safety: boolean;
  delete: boolean;
  move: boolean;
  astSearch: boolean;
  astReplace: boolean;
  lspDiagnostics: boolean;
  structure: boolean;
  refactor: boolean;
} {
  const surface = config.tool_surface ?? "recommended";
  const disabled = new Set(config.disabled_tools ?? []);
  const ok = (name: string): boolean => !disabled.has(name);
  const allOnly = (name: string): boolean => ALL_ONLY_TOOLS.has(name) && ok(name);

  if (surface === "minimal") {
    return {
      hoistRead: false,
      hoistWrite: false,
      hoistEdit: false,
      hoistGrep: false,
      outline: ok("aft_outline"),
      zoom: ok("aft_zoom"),
      semantic: false,
      navigate: false,
      conflicts: false,
      importTool: false,
      safety: ok("aft_safety"),
      delete: false,
      move: false,
      astSearch: false,
      astReplace: false,
      lspDiagnostics: false,
      structure: false,
      refactor: false,
    };
  }

  // recommended + all
  const base = {
    hoistRead: ok("read"),
    hoistWrite: ok("write"),
    hoistEdit: ok("edit"),
    hoistGrep: ok("grep") && config.experimental_search_index === true,
    outline: ok("aft_outline"),
    zoom: ok("aft_zoom"),
    semantic: ok("aft_search") && config.experimental_semantic_search === true,
    navigate: false,
    conflicts: ok("aft_conflicts"),
    importTool: ok("aft_import"),
    safety: ok("aft_safety"),
    delete: false,
    move: false,
    astSearch: ok("ast_grep_search"),
    astReplace: ok("ast_grep_replace"),
    lspDiagnostics: ok("lsp_diagnostics"),
    structure: false,
    refactor: false,
  };

  if (surface === "all") {
    return {
      ...base,
      navigate: allOnly("aft_navigate"),
      delete: allOnly("aft_delete"),
      move: allOnly("aft_move"),
      structure: allOnly("aft_transform"),
      refactor: allOnly("aft_refactor"),
    };
  }

  return base;
}

/**
 * Pi extension default export.
 *
 * Called once per session. Registers tools, commands, and session shutdown hooks.
 */
export default async function (pi: ExtensionAPI): Promise<void> {
  log(`AFT extension loading (plugin v${PLUGIN_VERSION})`);

  // Resolve AFT binary. On first run this downloads the platform binary to
  // ~/.cache/aft/bin/vX.Y.Z/aft. Failures bubble up as an error to Pi's loader.
  let binaryPath: string;
  try {
    binaryPath = await findBinary();
  } catch (err) {
    warn(
      `Failed to resolve AFT binary: ${err instanceof Error ? err.message : String(err)}. ` +
        "Tools will not be registered.",
    );
    return;
  }

  // Load config (user + project).
  const config = loadAftConfig(process.cwd());
  const storageDir = resolveStorageDir();

  // ONNX runtime for semantic search (optional, best-effort). `ensureOnnxRuntime`
  // handles unsupported platforms by returning null, so we don't need to pre-check.
  let ortDylibDir: string | null = null;
  if (config.experimental_semantic_search) {
    try {
      ortDylibDir = await ensureOnnxRuntime(storageDir);
      if (!ortDylibDir) {
        warn(
          `ONNX Runtime unavailable. Semantic search will be disabled. Install manually: ${getManualInstallHint()}`,
        );
      }
    } catch (err) {
      warn(`Failed to prepare ONNX Runtime: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  // Build configure-time overrides forwarded to every bridge on spawn.
  // Default to restrict_to_project_root: true for plugin-hosted agents.
  // The Rust CLI default is false (documented — for direct/scripted use), but
  // when agents call `aft_outline`, `aft_read`, etc. through the plugin there
  // is no interactive permission prompt for reads, so we must enforce the
  // project-root boundary by default. Users can opt out by explicitly setting
  // `restrict_to_project_root: false` in their aft.jsonc.
  const configOverrides: Record<string, unknown> = {
    ...config,
    ...resolveLspConfigForConfigure(config),
    restrict_to_project_root: config.restrict_to_project_root ?? true,
    storage_dir: storageDir,
  };
  delete configOverrides.lsp;
  if (ortDylibDir) {
    (configOverrides as Record<string, unknown>)._ort_dylib_dir = ortDylibDir;
  }

  const pool = new BridgePool(
    binaryPath,
    {
      minVersion: PLUGIN_VERSION,
      onConfigureWarnings: async ({ projectRoot, sessionId, client, warnings }) => {
        if (!sessionId || !client) return;
        const validWarnings = coerceConfigureWarnings(warnings);
        if (validWarnings.length === 0) return;
        await deliverConfigureWarnings(
          {
            client,
            sessionId,
            storageDir,
            pluginVersion: PLUGIN_VERSION,
            projectRoot,
          },
          validWarnings,
        );
      },
    },
    configOverrides,
  );
  const ctx: PluginContext = { pool, config, storageDir };

  const surface = resolveToolSurface(config);

  // Hoisted tool overrides (replace Pi's built-in read/write/edit/grep with AFT versions).
  registerHoistedTools(pi, ctx, surface);

  // AFT-specific tools
  if (surface.outline || surface.zoom) {
    registerReadingTools(pi, ctx, surface);
  }
  if (surface.semantic) {
    registerSemanticTool(pi, ctx);
  }
  if (surface.navigate) {
    registerNavigateTool(pi, ctx);
  }
  if (surface.conflicts) {
    registerConflictsTool(pi, ctx);
  }
  if (surface.importTool) {
    registerImportTools(pi, ctx);
  }
  if (surface.safety) {
    registerSafetyTool(pi, ctx);
  }
  if (surface.astSearch || surface.astReplace) {
    registerAstTools(pi, ctx, surface);
  }
  if (surface.delete || surface.move) {
    registerFsTools(pi, ctx, surface);
  }
  if (surface.lspDiagnostics) {
    registerLspTools(pi, ctx);
  }
  if (surface.structure) {
    registerStructureTool(pi, ctx);
  }
  if (surface.refactor) {
    registerRefactorTool(pi, ctx);
  }

  // Slash command: /aft-status
  registerStatusCommand(pi, ctx);

  // Clean up bridges on session shutdown.
  pi.on("session_shutdown", async () => {
    try {
      await pool.shutdown();
      log("Bridge pool shut down");
    } catch (err) {
      warn(`Error during bridge shutdown: ${err instanceof Error ? err.message : String(err)}`);
    }
  });

  // Also register process-level signal handlers so children get an orderly
  // shutdown when Pi's host Node process is killed directly (terminal close,
  // Ctrl+C, OS shutdown) rather than through the session_shutdown lifecycle.
  registerShutdownCleanup(async () => {
    try {
      await pool.shutdown();
    } catch (err) {
      warn(`Error during process shutdown: ${err instanceof Error ? err.message : String(err)}`);
    }
  });

  log(`AFT extension ready (surface=${config.tool_surface ?? "recommended"})`);
}
