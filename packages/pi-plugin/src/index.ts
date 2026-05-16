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
import {
  BridgePool,
  ensureBinary,
  ensureOnnxRuntime,
  findBinary,
  getManualInstallHint,
  isHomeDirectoryRoot,
  setActiveLogger,
} from "@cortexkit/aft-bridge";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
  appendToolResultBgCompletions,
  handlePushedBgCompletion,
  handlePushedBgLongRunning,
  handleTurnEndBgCompletions,
} from "./bg-notifications.js";
import { registerStatusCommand } from "./commands/aft-status.js";
import {
  type AftConfig,
  loadAftConfig,
  resolveExperimentalConfigForConfigure,
  resolveLspConfigForConfigure,
} from "./config.js";
import { bridgeLogger, error, log, warn } from "./logger.js";
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

// Register our logger with @cortexkit/aft-bridge before any bridge code runs.
setActiveLogger(bridgeLogger);

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

type BashLongRunningPayload = {
  session_id: string;
  task_id: string;
  command: string;
  elapsed_ms: number;
};

type BridgePendingState = {
  hasPendingRequests(): boolean;
};

type VersionMismatchPool = {
  replaceBinary(path: string): Promise<void>;
};

function createVersionMismatchHandler(
  getPool: () => VersionMismatchPool | undefined,
  ensureCompatibleBinary: (version?: string) => Promise<string | null> = ensureBinary,
) {
  // Track which binary version we already attempted to upgrade from.
  // Prevents the loop: mismatch → fire-and-forget download → replaceBinary kills bridge →
  // respawn with same binary → mismatch fires again → kills again → 3-attempt limit.
  let versionUpgradeAttempted: string | null = null;

  return (binaryVersion: string, minVersion: string) => {
    if (versionUpgradeAttempted === binaryVersion) {
      log(`Version ${binaryVersion} < ${minVersion} but upgrade already attempted — continuing`);
      return;
    }
    versionUpgradeAttempted = binaryVersion;
    warn(
      `WARNING: aft binary v${binaryVersion} is older than plugin v${minVersion}. ` +
        "Some features may not work. Attempting to download a compatible binary...",
    );
    // Fire-and-forget: try to download matching version and hot-swap future bridge spawns.
    ensureCompatibleBinary(`v${minVersion}`).then(
      (path) => {
        if (!path) {
          warn(`Could not find or download v${minVersion}. Continuing with v${binaryVersion}.`);
          return;
        }
        const pool = getPool();
        if (!pool) {
          warn(`Found/downloaded compatible binary at ${path}, but bridge pool is not ready.`);
          return;
        }
        log(`Found/downloaded compatible binary at ${path}. Replacing running bridges...`);
        pool.replaceBinary(path).then(
          () => {
            log("Binary replaced successfully. New bridges will use the updated binary.");
          },
          (err) => error("Failed to replace binary:", err),
        );
      },
      (err) => {
        error(
          `Auto-download failed: ${(err as Error).message}. Install manually: cargo install agent-file-tools@${minVersion}`,
        );
      },
    );
  };
}

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

const pendingEagerWarnings = new Map<string, ConfigureWarning[]>();

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

function drainPendingEagerWarnings(projectRoot: string): ConfigureWarning[] {
  const pending = pendingEagerWarnings.get(projectRoot) ?? [];
  pendingEagerWarnings.delete(projectRoot);
  return pending;
}

function shouldPrepareOnnxRuntime(
  config: Pick<AftConfig, "semantic_search" | "semantic">,
): boolean {
  const isFastembedSemanticBackend = (config.semantic?.backend ?? "fastembed") === "fastembed";
  return config.semantic_search === true && isFastembedSemanticBackend;
}

// IMPORTANT: NOT exported as a named export — only via the __test__
// namespace at the bottom. Pi's extension loader is different from
// OpenCode's, but OpenCode's plugin loader walks every top-level
// function export and treats them all as plugin entrypoints, which
// crashed our OpenCode-side plugin. Keeping both packages' surface
// shape identical avoids cross-contamination if shared utilities ever
// move between them.
async function handleConfigureWarningsForSession(context: {
  projectRoot: string;
  sessionId?: string | null;
  client?: unknown;
  warnings: unknown[];
  storageDir: string;
  pluginVersion: string;
}): Promise<void> {
  const validWarnings = coerceConfigureWarnings(context.warnings);

  if (!context.sessionId) {
    if (validWarnings.length === 0) return;
    const pending = pendingEagerWarnings.get(context.projectRoot) ?? [];
    pending.push(...validWarnings);
    pendingEagerWarnings.set(context.projectRoot, pending);
    warn(
      `[configure] deferred warnings for ${context.projectRoot} arrived without session_id; buffering until first session-bound call`,
    );
    return;
  }
  if (!context.client) {
    warn(
      `[configure] deferred warnings for session ${context.sessionId} arrived without notification client; skipping notification`,
    );
    return;
  }
  const pendingWarnings = drainPendingEagerWarnings(context.projectRoot);
  const combinedWarnings = [...pendingWarnings, ...validWarnings];
  if (combinedWarnings.length === 0) return;
  await deliverConfigureWarnings(
    {
      client: context.client,
      sessionId: context.sessionId,
      storageDir: context.storageDir,
      pluginVersion: context.pluginVersion,
      projectRoot: context.projectRoot,
    },
    combinedWarnings,
  );
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
    binaryPath = await findBinary(PLUGIN_VERSION);
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

  // ONNX runtime for semantic search (optional, best-effort).
  //
  // We deliberately do NOT block plugin load on this. The ONNX runtime archive
  // is 60–80 MB and on a slow connection this can take 30–120 seconds.
  // Awaiting it inline used to make Pi appear to hang during plugin load, and
  // SIGKILL'ing the host mid-download left partial state on disk that the
  // next launch had to recover from.
  //
  // Instead: kick off the download as a background promise and patch
  // `_ort_dylib_dir` into the pool's configure overrides as soon as it
  // settles. Bridges spawned AFTER the download finishes pick it up
  // automatically. `ensureOnnxRuntime` returns null on unsupported platforms.
  let onnxRuntimePromise: Promise<string | null> | null = null;
  if (shouldPrepareOnnxRuntime(config)) {
    onnxRuntimePromise = ensureOnnxRuntime(storageDir).catch((err) => {
      warn(`Failed to prepare ONNX Runtime: ${err instanceof Error ? err.message : String(err)}`);
      return null;
    });
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
  // _ort_dylib_dir is patched in asynchronously below once ensureOnnxRuntime
  // settles. Bridges spawned before that resolution don't get ORT and
  // semantic search returns "still building" until they restart.

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

  let pool: BridgePool;
  const poolOptions: import("@cortexkit/aft-bridge").PoolOptions & {
    onBashLongRunning: (reminder: BashLongRunningPayload, bridge: BridgePendingState) => void;
  } = {
    errorPrefix: "[aft-pi]",
    minVersion: PLUGIN_VERSION,
    onVersionMismatch: createVersionMismatchHandler(() => pool),
    onConfigureWarnings: async ({ projectRoot, sessionId, client, warnings }) => {
      const pendingWarnings = sessionId ? drainPendingEagerWarnings(projectRoot) : [];
      await handleConfigureWarningsForSession({
        projectRoot,
        sessionId,
        client,
        warnings: [...pendingWarnings, ...warnings],
        storageDir,
        pluginVersion: PLUGIN_VERSION,
      });
    },
    onBashCompletion: (completion) => {
      void handlePushedBgCompletion(
        {
          ctx,
          directory: process.cwd(),
          sessionID: completion.session_id,
          runtime: pi,
        },
        completion,
      );
    },
    onBashLongRunning: (reminder) => {
      void handlePushedBgLongRunning(
        {
          ctx,
          directory: process.cwd(),
          sessionID: reminder.session_id,
          runtime: pi,
        },
        reminder,
      );
    },
  };
  pool = new BridgePool(binaryPath, poolOptions, configOverrides);
  const ctx: PluginContext = { pool, config, storageDir };

  // Settle the ONNX runtime download promise (started above) and patch the
  // resolved path into the pool's configure overrides. Bridges spawned AFTER
  // this resolves will pass `_ort_dylib_dir` through configure and pick up
  // the runtime; bridges already running at resolution time keep going
  // without ORT (we don't restart them — that would discard warm
  // trigram/semantic/LSP state). Result: semantic search becomes available
  // for new sessions automatically once the download completes.
  if (onnxRuntimePromise) {
    onnxRuntimePromise.then(
      (ortDylibDir) => {
        if (ortDylibDir) {
          pool.setConfigureOverride("_ort_dylib_dir", ortDylibDir);
          log(`ONNX Runtime ready at ${ortDylibDir}; new bridges will load semantic backend.`);
        } else {
          warn(
            `ONNX Runtime unavailable. Semantic search will be disabled. Install manually: ${getManualInstallHint()}`,
          );
        }
      },
      (err) => {
        warn(`ONNX Runtime resolution rejected unexpectedly: ${err}`);
      },
    );
  }

  // Eager async configure: warm the bridge for `process.cwd()` so the first
  // tool call doesn't pay the spawn + configure latency. Errors are swallowed —
  // the next real tool call will surface a proper error.
  void (async () => {
    try {
      // Note #65: skip eager configure when Pi was launched from the user's
      // home directory. Configuring on `$HOME` walks the entire user home
      // tree (100k–10M files), times out the 30s configure budget, gets
      // killed, then silently retries on every reload. The first real tool
      // call from a session will still warm the correct project bridge.
      const cwd = process.cwd();
      if (isHomeDirectoryRoot(cwd)) {
        log(
          `Eager configure skipped: cwd=${cwd} is the user home directory. ` +
            `The first real tool call will warm the correct project bridge.`,
        );
        return;
      }
      // Await ONNX runtime resolution BEFORE spawning the bridge — otherwise
      // the bridge starts without _ort_dylib_dir on its configure overrides,
      // and Rust falls back to a system-path dlopen("libonnxruntime.dylib")
      // that almost always fails on macOS / Windows (the user has the runtime
      // managed in `<storage_dir>/onnxruntime/`, not on the system loader
      // path). The race symptom in the wild: log shows
      //   Spawning binary: ...
      //   ONNX Runtime ready at ...   <- 4ms later
      //   failed to build semantic index: ONNX Runtime not found
      // because the bridge spawn at t=0 has no _ort_dylib_dir yet, and once
      // it's set on the pool only NEW bridges pick it up. Mirror the
      // OpenCode plugin: cap at 60s so a slow/broken download doesn't block
      // the warmup permanently; the bridge still spawns without ORT after
      // the cap and semantic just fails honestly.
      if (onnxRuntimePromise) {
        await Promise.race([
          onnxRuntimePromise,
          new Promise<null>((resolve) => setTimeout(() => resolve(null), 60_000)),
        ]);
      }
      const bridge = pool.getBridge(cwd);
      // No session_id: runs before any user session exists; configure
      // threads spawned by this warmup will log with no [ses_xxx] prefix.
      const response = await bridge.send("status", {});
      // Seed the plugin-side cache so the /aft-status overlay's first poll
      // after spawn finds a warm snapshot instead of racing into bridge.send
      // and hitting the client timeout while the bridge dispatch loop is
      // still finishing configure. Push frames will overwrite this with
      // fresh data on every state transition (1s debounce).
      if (response.success !== false) {
        bridge.cacheStatusSnapshot(response as Parameters<typeof bridge.cacheStatusSnapshot>[0]);
      }
    } catch (err) {
      log(`eager configure failed: ${err instanceof Error ? err.message : String(err)}`);
    }
  })();

  if (ANNOUNCEMENT_VERSION && ANNOUNCEMENT_FEATURES.length > 0) {
    sendFeatureAnnouncement(ANNOUNCEMENT_VERSION, ANNOUNCEMENT_FEATURES, storageDir);
  }

  const surface = resolveToolSurface(config);

  // Hoisted tool overrides (replace Pi's built-in bash/read/write/edit/grep with AFT versions).
  // Bash hoisting is opt-in: only register the AFT bash replacement when the
  // user has enabled at least one experimental.bash.* flag (rewrite, compress,
  // or background). When all flags are off, Pi's native bash stays in place
  // and no AFT bash code is in the path. registerBashTool handles per-flag
  // gating internally for bash_status / bash_kill.
  // Read nested user-facing config shape — the flat experimental_bash_* keys
  // are an internal Rust-configure detail, not the surface users write to.
  const anyBashExperimental =
    config.experimental?.bash?.rewrite === true ||
    config.experimental?.bash?.compress === true ||
    config.experimental?.bash?.background === true;
  if (surface.hoistBash && anyBashExperimental) {
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

export const __test__ = {
  resolveToolSurface,
  handleConfigureWarningsForSession,
  shouldPrepareOnnxRuntime,
  createVersionMismatchHandler,
};
