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
 *   - aft_search     Semantic search (when semantic_search=true)
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
import {
  appendToolResultBgCompletions,
  handlePushedBgCompletion,
  handleTurnEndBgCompletions,
  resetBgWake,
} from "./bg-notifications.js";
import { registerStatusCommand } from "./commands/aft-status.js";
import {
  loadAftConfig,
  resolveExperimentalConfigForConfigure,
  resolveLspConfigForConfigure,
} from "./config.js";
import { log, warn } from "./logger.js";
import { abortInFlightAutoInstalls, runAutoInstall } from "./lsp-auto-install.js";
import {
  abortInFlightGithubInstalls,
  discoverRelevantGithubServers,
  runGithubAutoInstall,
} from "./lsp-github-install.js";
import { GITHUB_LSP_TABLE } from "./lsp-github-table.js";
import { NPM_LSP_TABLE } from "./lsp-npm-table.js";
import {
  type ConfigureWarning,
  deliverConfigureWarnings,
  sendFeatureAnnouncement,
} from "./notifications.js";
import { ensureOnnxRuntime, getManualInstallHint } from "./onnx-runtime.js";
import { BridgePool } from "./pool.js";
import { findBinary } from "./resolver.js";
import { registerShutdownCleanup } from "./shutdown-hooks.js";
import { resolveSessionId } from "./tools/_shared.js";
import { registerAstTools } from "./tools/ast.js";
import { registerBashTool } from "./tools/bash.js";
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
import { registerWorkflowHints } from "./workflow-hints.js";

/** Plugin version from package.json. */
const PLUGIN_VERSION: string = (() => {
  try {
    const req = createRequire(import.meta.url);
    return (req("../package.json") as { version: string }).version;
  } catch {
    return "0.0.0";
  }
})();

const ANNOUNCEMENT_VERSION = "0.18.0";
const ANNOUNCEMENT_FEATURES: string[] = [
  "New experimental features — AFT now optionally hoists bash:\n    - Run bash scripts in the background.\n    - Initial output compression for git, cargo, npm, bun, pnpm, pytest, tsc (more in 0.19).\n    - Rewrite cat/grep/find/sed/ls into AFT counterparts for faster, formatted output.\n  Check GitHub for how to enable.",
  "Trigram grep/glob and semantic search (aft_search) graduated out of experimental.",
  "Lots of bugfixes and new end-to-end test coverage.",
];

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
  hoistBash: boolean;
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
      hoistBash: ok("bash"),
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
    hoistBash: ok("bash"),
    hoistRead: ok("read"),
    hoistWrite: ok("write"),
    hoistEdit: ok("edit"),
    hoistGrep: ok("grep") && config.search_index === true,
    outline: ok("aft_outline"),
    zoom: ok("aft_zoom"),
    semantic: ok("aft_search") && config.semantic_search === true,
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
  if (config.semantic_search) {
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
  //
  // STRICT ALLOWLIST (audit v0.17 #18): we explicitly pick fields from
  // `config` instead of spreading. The previous `...config` spread leaked
  // every top-level field — including `restrict_to_project_root` and
  // `url_fetch_allow_private` — through the project-config trust boundary,
  // because at this point `config` is the merged user+project view and
  // mergeConfigs alone is not enough.
  //
  // Default `restrict_to_project_root: false` for parity with Pi's built-in
  // tools, which do NOT enforce a project-root boundary at all (Pi's
  // `resolveToCwd` resolves absolute paths through unchanged). AFT previously
  // defaulted to `true`, hard-rejecting out-of-root paths that Pi's own
  // tools would have happily processed. Users who want strict containment
  // can opt in by setting `restrict_to_project_root: true` in their aft.jsonc
  // (USER config only; project config cannot weaken this — see trust
  // boundary in config.ts).
  const configOverrides: Record<string, unknown> = {};
  if (config.format_on_edit !== undefined) configOverrides.format_on_edit = config.format_on_edit;
  if (config.formatter_timeout_secs !== undefined)
    configOverrides.formatter_timeout_secs = config.formatter_timeout_secs;
  if (config.validate_on_edit !== undefined)
    configOverrides.validate_on_edit = config.validate_on_edit;
  if (config.formatter !== undefined) configOverrides.formatter = config.formatter;
  if (config.checker !== undefined) configOverrides.checker = config.checker;
  configOverrides.restrict_to_project_root = config.restrict_to_project_root ?? false;
  if (config.search_index !== undefined) configOverrides.search_index = config.search_index;
  if (config.semantic_search !== undefined)
    configOverrides.semantic_search = config.semantic_search;
  Object.assign(configOverrides, resolveExperimentalConfigForConfigure(config));
  Object.assign(configOverrides, resolveLspConfigForConfigure(config));
  if (config.semantic !== undefined) configOverrides.semantic = config.semantic;
  if (config.max_callgraph_files !== undefined)
    configOverrides.max_callgraph_files = config.max_callgraph_files;
  // url_fetch_allow_private: USER ONLY. Forwarded only when set (Rust default false).
  if (config.url_fetch_allow_private !== undefined)
    configOverrides.url_fetch_allow_private = config.url_fetch_allow_private;
  configOverrides.storage_dir = storageDir;
  if (ortDylibDir) {
    configOverrides._ort_dylib_dir = ortDylibDir;
  }

  // ─────────────────────────── LSP auto-install ───────────────────────────
  // Mirrors the OpenCode plugin: discover relevant LSPs, surface cached bin
  // dirs to Rust as `lsp_paths_extra`, kick off background installs for
  // anything missing. The 7-day grace defends against newly-published
  // malicious versions. Best-effort — failures never block plugin startup.
  try {
    const lspAutoInstall = config.lsp?.auto_install ?? true;
    const lspGraceDays = config.lsp?.grace_days ?? 7;
    const lspVersions = config.lsp?.versions ?? {};
    const lspDisabled = new Set(config.lsp?.disabled ?? []);
    const projectRoot = process.cwd();
    configOverrides.lsp_auto_install_binaries = [
      ...new Set([...NPM_LSP_TABLE, ...GITHUB_LSP_TABLE].map((spec) => spec.binary)),
    ];

    const npmResult = runAutoInstall(projectRoot, {
      autoInstall: lspAutoInstall,
      graceDays: lspGraceDays,
      versions: lspVersions,
      disabled: lspDisabled,
    });
    const relevantGithub = discoverRelevantGithubServers(projectRoot);
    const ghResult = runGithubAutoInstall(relevantGithub, {
      autoInstall: lspAutoInstall,
      graceDays: lspGraceDays,
      versions: lspVersions,
      disabled: lspDisabled,
    });
    const mergedBinDirs = [...npmResult.cachedBinDirs, ...ghResult.cachedBinDirs];
    if (mergedBinDirs.length > 0) {
      configOverrides.lsp_paths_extra = mergedBinDirs;
    }
    const lspInflightInstalls = [
      ...new Set([...npmResult.installingBinaries, ...ghResult.installingBinaries]),
    ];
    if (lspInflightInstalls.length > 0) {
      configOverrides.lsp_inflight_installs = lspInflightInstalls;
    }
    if (npmResult.installsStarted > 0 || ghResult.installsStarted > 0) {
      log(
        `[lsp] auto-install: ${npmResult.installsStarted} npm + ${ghResult.installsStarted} github install(s) running in background`,
      );
    }

    // ─── Surface install outcomes once installs settle (audit #6) ───
    //
    // Pi loads this extension once at startup, before any session exists, so
    // we can't send an ignored session message the way the OpenCode plugin
    // does. Instead we promote actionable skips from `log()` (verbose) to
    // `warn()` (visible at WARN level) so users running with default logging
    // see them. Routine skips (already-installed, not-relevant, disabled)
    // stay out of the warning summary.
    Promise.all([npmResult.installsComplete, ghResult.installsComplete])
      .then(() => {
        const actionable = [...npmResult.skipped, ...ghResult.skipped].filter((s) => {
          const r = s.reason.toLowerCase();
          if (r === "auto_install: false") return false;
          if (r === "disabled by config") return false;
          if (r === "not relevant to project") return false;
          if (r === "already installed") return false;
          if (r === "another install in progress") return false;
          return true;
        });
        if (actionable.length === 0) return;
        const lines = actionable.map((s) => `  • ${s.id}: ${s.reason}`).join("\n");
        warn(
          `[lsp] skipped or failed to install ${actionable.length} server(s):\n${lines}\n` +
            'Pin a working version with `lsp.versions: { "<package>": "<version>" }` if grace is blocking, ' +
            "or set `lsp.auto_install: false` to suppress.",
        );
      })
      .catch((err) => {
        warn(`[lsp] install-summary aggregation failed: ${err}`);
      });
  } catch (err) {
    warn(`[lsp] auto-install setup failed: ${err instanceof Error ? err.message : String(err)}`);
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
      onBashCompletion: (completion, bridge) => {
        void handlePushedBgCompletion(
          {
            ctx,
            directory: process.cwd(),
            sessionID: completion.session_id,
            runtime: pi,
            isActive: () => bridge.hasPendingRequests(),
          },
          completion,
        );
      },
    },
    configOverrides,
  );
  const ctx: PluginContext = { pool, config, storageDir };

  if (ANNOUNCEMENT_VERSION && ANNOUNCEMENT_FEATURES.length > 0) {
    sendFeatureAnnouncement(ANNOUNCEMENT_VERSION, ANNOUNCEMENT_FEATURES, storageDir);
  }

  const surface = resolveToolSurface(config);

  // Hoisted tool overrides (replace Pi's built-in bash/read/write/edit/grep with AFT versions).
  if (surface.hoistBash) {
    registerBashTool(pi, ctx);
  }
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

  // Workflow hints: short system-prompt block teaching token-efficient
  // AFT workflows. Hooked into Pi's `before_agent_start` event with
  // systemPrompt extension. Always-on; conditional on the registered
  // tool surface so absent tools aren't advertised.
  registerWorkflowHints(pi, config, surface);

  // Slash command: /aft-status
  registerStatusCommand(pi, ctx);

  (
    pi.on as (
      event: "tool_result",
      handler: (
        event: {
          content: Array<
            { type: "text"; text: string } | { type: "image"; data: string; mimeType: string }
          >;
          details: unknown;
          isError: boolean;
        },
        ctx: Parameters<typeof resolveSessionId>[0] & { cwd: string },
      ) => unknown,
    ) => void
  )("tool_result", async (event, extCtx) => {
    const content = await appendToolResultBgCompletions(
      { ctx, directory: extCtx.cwd, sessionID: resolveSessionId(extCtx) },
      event.content,
    );
    if (!content) return undefined;
    return { content, details: event.details, isError: event.isError };
  });

  (
    pi.on as (
      event: "turn_end",
      handler: (
        event: unknown,
        ctx: Parameters<typeof resolveSessionId>[0] & { cwd: string },
      ) => unknown,
    ) => void
  )("turn_end", async (_event, extCtx) => {
    await handleTurnEndBgCompletions({
      ctx,
      directory: extCtx.cwd,
      sessionID: resolveSessionId(extCtx),
      runtime: pi,
    });
  });

  pi.on("input", (_event, extCtx) => {
    resetBgWake(resolveSessionId(extCtx));
    return { action: "continue" };
  });

  // Clean up bridges on session shutdown.
  pi.on("session_shutdown", async () => {
    try {
      await Promise.allSettled([abortInFlightAutoInstalls(), abortInFlightGithubInstalls()]);
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
      await Promise.allSettled([abortInFlightAutoInstalls(), abortInFlightGithubInstalls()]);
      await pool.shutdown();
    } catch (err) {
      warn(`Error during process shutdown: ${err instanceof Error ? err.message : String(err)}`);
    }
  });

  log(`AFT extension ready (surface=${config.tool_surface ?? "recommended"})`);
}

export const __test__ = { resolveToolSurface };
