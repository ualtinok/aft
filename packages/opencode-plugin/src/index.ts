import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import type { Plugin } from "@opencode-ai/plugin";

import {
  appendInTurnBgCompletions,
  extractSessionID,
  handleIdleBgCompletions,
  resetBgWake,
} from "./bg-notifications.js";
import {
  loadAftConfig,
  resolveExperimentalConfigForConfigure,
  resolveLspConfigForConfigure,
} from "./config.js";
import { ensureBinary } from "./downloader.js";
import { createAutoUpdateCheckerHook } from "./hooks/auto-update-checker/index.js";
import { error, log, warn } from "./logger.js";
import { abortInFlightAutoInstalls, runAutoInstall } from "./lsp-auto-install.js";
import {
  abortInFlightGithubInstalls,
  discoverRelevantGithubServers,
  runGithubAutoInstall,
} from "./lsp-github-install.js";
import { consumeToolMetadata } from "./metadata-store.js";
import { normalizeToolMap } from "./normalize-schemas.js";
import {
  type ConfigureWarning,
  cleanupWarnings,
  deliverConfigureWarnings,
  type NotificationOptions,
  sendFeatureAnnouncement,
  sendWarning,
} from "./notifications.js";
import {
  ensureOnnxRuntime,
  getManualInstallHint,
  isOrtAutoDownloadSupported,
} from "./onnx-runtime.js";
import { BridgePool } from "./pool.js";
import { findBinary } from "./resolver.js";
import { AftRpcServer } from "./shared/rpc-server.js";
import { clearSharedBridgePool, setSharedBridgePool } from "./shared/runtime.js";
import { coerceAftStatus, formatStatusMarkdown } from "./shared/status.js";
import { ensureTuiPluginEntry } from "./shared/tui-config.js";
import { cleanupUrlCache } from "./shared/url-fetch.js";
import { registerShutdownCleanup } from "./shutdown-hooks.js";
import { astTools } from "./tools/ast.js";
import { conflictTools } from "./tools/conflicts.js";
import { aftPrefixedTools, hoistedTools } from "./tools/hoisted.js";
import { importTools } from "./tools/imports.js";
import { lspTools } from "./tools/lsp.js";
import { navigationTools } from "./tools/navigation.js";
import { readingTools } from "./tools/reading.js";
import { refactoringTools } from "./tools/refactoring.js";
import { safetyTools } from "./tools/safety.js";
import { searchTools } from "./tools/search.js";
import { semanticTools } from "./tools/semantic.js";
import { structureTools } from "./tools/structure.js";
import type { PluginContext } from "./types.js";

const STATUS_COMMAND = "aft-status";
const SENTINEL_PREFIX = "__AFT_STATUS_";

function isTuiMode(): boolean {
  return process.env.OPENCODE_CLIENT === "cli";
}

// Slash commands are registered by the TUI plugin (tui/index.tsx) via api.command.register()
// which works in both TUI and Desktop modes. The server plugin only handles execution
// via command.execute.before hook (for Desktop rendering as ignored message).

function throwSentinel(command: string): never {
  throw new Error(`${SENTINEL_PREFIX}${command.toUpperCase().replace(/-/g, "_")}_HANDLED__`);
}

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

async function sendIgnoredMessage(client: unknown, sessionID: string, text: string): Promise<void> {
  const typedClient = client as {
    session?: {
      prompt?: (input: unknown) => unknown;
      promptAsync?: (input: unknown) => unknown;
    };
  };

  const promptInput = {
    path: { id: sessionID },
    body: {
      noReply: true,
      parts: [{ type: "text", text, ignored: true }],
    },
  };

  if (typeof typedClient.session?.prompt === "function") {
    await Promise.resolve(typedClient.session.prompt(promptInput));
    return;
  }

  if (typeof typedClient.session?.promptAsync === "function") {
    await typedClient.session.promptAsync(promptInput);
    return;
  }

  throw new Error("[aft-plugin] client.session.prompt is unavailable");
}

/** Read the plugin's own version from package.json at build time. */
const PLUGIN_VERSION: string = (() => {
  try {
    const req = createRequire(import.meta.url);
    return (req("../package.json") as { version: string }).version;
  } catch {
    return "0.0.0";
  }
})();

/**
 * Release-notes identifier for the startup announcement dialog.
 *
 * This is intentionally decoupled from PLUGIN_VERSION so bugfix releases don't
 * re-trigger a stale dialog. Bump this string and populate ANNOUNCEMENT_FEATURES
 * ONLY when a release ships user-facing news worth surfacing once at startup.
 * Leave ANNOUNCEMENT_VERSION empty (or ANNOUNCEMENT_FEATURES empty) to skip the
 * dialog entirely for bugfix-only releases.
 *
 * Persistence (storage/last_announced_version) stores this value, so once a user
 * dismisses an announcement, patch releases that don't bump ANNOUNCEMENT_VERSION
 * will not re-show it.
 */
const ANNOUNCEMENT_VERSION = "0.18.0";
const ANNOUNCEMENT_FEATURES: string[] = [
  "New experimental features — AFT now optionally hoists bash:\n    - Run bash scripts in the background.\n    - Initial output compression for git, cargo, npm, bun, pnpm, pytest, tsc (more in 0.19).\n    - Rewrite cat/grep/find/sed/ls into AFT counterparts for faster, formatted output.\n  Check GitHub for how to enable.",
  "Trigram grep/glob and semantic search (aft_search) graduated out of experimental.",
  "Lots of bugfixes and new end-to-end test coverage.",
];

/**
 * AFT (Agent File Toolkit) plugin for OpenCode.
 *
 * Config is loaded from two levels (project overrides user):
 * - User:    ~/.config/opencode/aft.jsonc (or .json)
 * - Project: <project>/.opencode/aft.jsonc (or .json)
 *
 * Tools organized into groups:
 * - Hoisted (default): read, write, edit, apply_patch, ast_grep_search, ast_grep_replace
 *   and grep/glob when search_index is enabled
 * - File ops: aft_delete, aft_move
 * - Reading: aft_outline
 * - Safety: aft_safety
 * - Imports: aft_import
 * - Structure: aft_transform
 * - Navigation: aft_navigate
 * - Refactoring: aft_refactor
 * - LSP: aft_lsp_diagnostics (inline diagnostics on edits are automatic)
 */
const plugin: Plugin = async (input) => {
  const binaryPath = await findBinary();

  // Load config: ~/.config/opencode/aft.jsonc → <project>/.opencode/aft.jsonc
  const aftConfig = loadAftConfig(input.directory);
  const autoUpdateAbort = new AbortController();

  // Build config overrides for the Rust binary (strip undefined values)
  const configOverrides: Record<string, unknown> = {};
  if (aftConfig.format_on_edit !== undefined)
    configOverrides.format_on_edit = aftConfig.format_on_edit;
  if (aftConfig.validate_on_edit !== undefined)
    configOverrides.validate_on_edit = aftConfig.validate_on_edit;
  if (aftConfig.formatter !== undefined) configOverrides.formatter = aftConfig.formatter;
  if (aftConfig.checker !== undefined) configOverrides.checker = aftConfig.checker;
  // Default to restrict_to_project_root: true for plugin-hosted agents.
  // The Rust CLI default is false (documented — for direct/scripted use), but
  // when agents call `aft_outline`, `aft_read`, etc. through the plugin there
  // is no interactive permission prompt for reads, so we must enforce the
  // project-root boundary by default. Users can opt out by explicitly setting
  // `restrict_to_project_root: false` in their aft.jsonc.
  configOverrides.restrict_to_project_root = aftConfig.restrict_to_project_root ?? true;
  configOverrides.bash_permissions = true;
  if (aftConfig.search_index !== undefined) configOverrides.search_index = aftConfig.search_index;
  if (aftConfig.semantic_search !== undefined)
    configOverrides.semantic_search = aftConfig.semantic_search;
  Object.assign(configOverrides, resolveExperimentalConfigForConfigure(aftConfig));
  Object.assign(configOverrides, resolveLspConfigForConfigure(aftConfig));
  if (aftConfig.semantic !== undefined) configOverrides.semantic = aftConfig.semantic;
  if (aftConfig.max_callgraph_files !== undefined)
    configOverrides.max_callgraph_files = aftConfig.max_callgraph_files;

  const isFastembedSemanticBackend = (aftConfig.semantic?.backend ?? "fastembed") === "fastembed";

  // Compute XDG-compliant storage dir for persistent indexes (trigram, semantic)
  // Pattern: ~/.local/share/opencode/storage/plugin/aft/
  const dataHome = process.env.XDG_DATA_HOME || join(homedir(), ".local", "share");
  configOverrides.storage_dir = join(dataHome, "opencode", "storage", "plugin", "aft");

  // Auto-resolve ONNX Runtime for semantic search.
  // Downloads the shared library on first use if the platform is supported.
  // The resolved path is passed to bridges via ORT_DYLIB_PATH env var.
  if (aftConfig.semantic_search && isFastembedSemanticBackend) {
    const storageDir = configOverrides.storage_dir as string;
    const ortDylibDir = await ensureOnnxRuntime(storageDir).catch((err) => {
      warn(
        `ONNX Runtime setup failed: ${err instanceof Error ? err.message : String(err)}. Semantic search will be unavailable.`,
      );
      return null;
    });
    if (ortDylibDir) {
      configOverrides._ort_dylib_dir = ortDylibDir;
    } else if (!isOrtAutoDownloadSupported()) {
      warn(`Semantic search requires ONNX Runtime. Install: ${getManualInstallHint()}`);
    }
  }

  // ─────────────────────────── LSP auto-install ───────────────────────────
  //
  // Discover which LSPs the project actually needs, then surface every
  // already-cached binary directory to Rust as `lsp_paths_extra`. The Rust
  // resolver checks this list (after project-local node_modules and before
  // PATH), so any LSP we previously installed is found without users having
  // to put it on PATH.
  //
  // For LSPs that aren't yet cached, we kick off a background install (npm
  // for typescript-language-server / pyright / yaml-ls / bash-ls / dockerfile-ls
  // / @vue/language-server / @astrojs/language-server / svelte-language-server
  // / intelephense / @biomejs/biome; GitHub releases for clangd / lua-ls / zls
  // / tinymist / texlab). The 7-day grace window in `lsp.grace_days` defends
  // against newly-published malicious versions. Newly-installed binaries
  // appear in the cache for the user's NEXT plugin session — matching the
  // OpenCode "may need restart" UX and avoiding mid-session bridge restarts.
  //
  // The whole step is best-effort: if both probes fail, `cachedBinDirs` is
  // still populated from `isInstalled()` checks, so previously-installed
  // binaries continue to work.
  try {
    const lspAutoInstall = aftConfig.lsp?.auto_install ?? true;
    const lspGraceDays = aftConfig.lsp?.grace_days ?? 7;
    const lspVersions = aftConfig.lsp?.versions ?? {};
    const lspDisabled = new Set(aftConfig.lsp?.disabled ?? []);

    const npmResult = runAutoInstall(input.directory, {
      autoInstall: lspAutoInstall,
      graceDays: lspGraceDays,
      versions: lspVersions,
      disabled: lspDisabled,
    });

    // GitHub-distributed servers gate on relevance separately because the
    // binaries are heavier (10-100 MB).
    const relevantGithub = discoverRelevantGithubServers(input.directory);
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
    if (npmResult.installsStarted > 0 || ghResult.installsStarted > 0) {
      log(
        `[lsp] auto-install: ${npmResult.installsStarted} npm + ${ghResult.installsStarted} github install(s) running in background`,
      );
    }

    // ─── Surface install outcomes once installs settle (audit #6) ───
    //
    // Both `runAutoInstall` and `runGithubAutoInstall` return synchronously
    // with the obvious skips (disabled, irrelevant, auto_install: false). The
    // backgrounded installs append additional reasons (grace blocked, registry
    // probe failed, install crashed) into `skipped` as their promises settle.
    //
    // We deliver ONE consolidated ignored message per session listing only
    // actionable reasons — the user can act on "grace blocked" (set a pin) or
    // "install failed" (check `/aft-status` and the plugin log), but not on
    // "not relevant to project" or "already installed" which are routine.
    //
    // Fire-and-forget; never block plugin startup.
    Promise.all([npmResult.installsComplete, ghResult.installsComplete])
      .then(() => {
        const actionable = [...npmResult.skipped, ...ghResult.skipped].filter((s) => {
          const r = s.reason.toLowerCase();
          // Routine skips — don't notify.
          if (r === "auto_install: false") return false;
          if (r === "disabled by config") return false;
          if (r === "not relevant to project") return false;
          if (r === "already installed") return false;
          if (r === "another install in progress") return false;
          return true;
        });
        if (actionable.length === 0) return;

        const lines = actionable.map((s) => `  • ${s.id}: ${s.reason}`).join("\n");
        const message =
          `AFT skipped or failed to install ${actionable.length} LSP server(s):\n${lines}\n\n` +
          "See `/aft-status` for details, or check the plugin log. " +
          'Pin a working version with `lsp.versions: { "<package>": "<version>" }` if grace is blocking, ' +
          "or set `lsp.auto_install: false` to suppress this entirely.";
        sendWarning({ client: input.client, directory: input.directory }, message).catch((err) => {
          warn(`[lsp] failed to deliver install summary: ${err}`);
        });
      })
      .catch((err) => {
        warn(`[lsp] install-summary aggregation failed: ${err}`);
      });
  } catch (err) {
    // Auto-install failures must never block plugin startup.
    warn(`[lsp] auto-install setup failed: ${err instanceof Error ? err.message : String(err)}`);
  }

  // Track which binary version we already attempted to upgrade from.
  // Prevents the loop: mismatch → fire-and-forget download → replaceBinary kills bridge →
  // respawn with same binary → mismatch fires again → kills again → 3-attempt limit.
  let versionUpgradeAttempted: string | null = null;

  const pool = new BridgePool(
    binaryPath,
    {
      minVersion: PLUGIN_VERSION,
      onVersionMismatch: (binaryVersion, minVersion) => {
        if (versionUpgradeAttempted === binaryVersion) {
          log(
            `Version ${binaryVersion} < ${minVersion} but upgrade already attempted — continuing`,
          );
          return;
        }
        versionUpgradeAttempted = binaryVersion;
        warn(
          `WARNING: aft binary v${binaryVersion} is older than plugin v${minVersion}. ` +
            "Some features may not work. Attempting to download a compatible binary...",
        );
        // Fire-and-forget: try to download matching version and hot-swap
        ensureBinary(`v${minVersion}`).then(
          (path) => {
            if (path) {
              log(`Found/downloaded compatible binary at ${path}. Replacing running bridges...`);
              pool.replaceBinary(path).then(
                () => {
                  // Don't reset versionUpgradeAttempted here — the new binary might also be
                  // outdated. The tracker resets naturally when a new plugin version loads
                  // (fresh plugin init creates a new closure). This prevents re-triggering
                  // the same upgrade attempt on subsequent tool calls.
                  log("Binary replaced successfully. New bridges will use the updated binary.");
                },
                (err) => error("Failed to replace binary:", err),
              );
            } else {
              warn(`Could not find or download v${minVersion}. Continuing with v${binaryVersion}.`);
            }
          },
          (err) => {
            error(
              `Auto-download failed: ${(err as Error).message}. Install manually: cargo install agent-file-tools@${minVersion}`,
            );
          },
        );
      },
      onConfigureWarnings: async ({ projectRoot, sessionId, client, warnings }) => {
        if (!sessionId) return;
        const validWarnings = coerceConfigureWarnings(warnings);
        if (validWarnings.length === 0) return;
        await deliverConfigureWarnings(
          {
            client: client ?? input.client,
            sessionId,
            storageDir: configOverrides.storage_dir as string,
            pluginVersion: PLUGIN_VERSION,
            projectRoot,
          },
          validWarnings,
        );
      },
    },
    configOverrides,
  );
  const ctx: PluginContext = {
    pool,
    client: input.client,
    plugin: (input as { plugin?: PluginContext["plugin"] }).plugin,
    config: aftConfig,
    storageDir: configOverrides.storage_dir as string,
  };
  setSharedBridgePool(pool);

  // Start RPC server for TUI plugin communication
  const rpcServer = new AftRpcServer(configOverrides.storage_dir as string, input.directory);

  // Install process-level SIGTERM/SIGINT handlers so that child `aft` processes
  // get an orderly shutdown when the Node host receives a termination signal.
  // Without this, OS propagates SIGTERM to children before OpenCode calls dispose,
  // and (together with bridge.ts signal handling) we want the shutdown path we
  // control, not implicit process-group death. The returned unregister is called
  // from dispose so plugin reloads don't leak stale cleanup callbacks.
  const unregisterShutdown = registerShutdownCleanup(async () => {
    await Promise.allSettled([abortInFlightAutoInstalls(), abortInFlightGithubInstalls()]);
    try {
      rpcServer.stop();
    } catch {
      // best-effort
    }
    await pool.shutdown();
  });
  rpcServer.handle("status", async (params) => {
    const sessionID = (params.sessionID as string) || "rpc";
    // Prefer an already-warm bridge (semantic/trigram indexes loaded) before
    // spawning a cold one just to answer a status query.
    const bridge = pool.getAnyActiveBridge(input.directory) ?? pool.getBridge(input.directory);
    return await bridge.send("status", { session_id: sessionID });
  });
  // Feature announcement — TUI plugin calls this on startup to show a dialog.
  // Uses ANNOUNCEMENT_VERSION (not PLUGIN_VERSION) so patch releases don't re-fire.
  const storageDir = configOverrides.storage_dir as string;

  rpcServer.handle("get-announcement", async () => {
    if (!ANNOUNCEMENT_VERSION || ANNOUNCEMENT_FEATURES.length === 0) {
      return { show: false };
    }
    if (storageDir) {
      const versionFile = join(storageDir, "last_announced_version");
      try {
        if (existsSync(versionFile)) {
          const lastVersion = readFileSync(versionFile, "utf-8").trim();
          if (lastVersion === ANNOUNCEMENT_VERSION) return { show: false };
        }
      } catch {
        // proceed
      }
    }
    return { show: true, version: ANNOUNCEMENT_VERSION, features: ANNOUNCEMENT_FEATURES };
  });

  rpcServer.handle("mark-announced", async () => {
    if (storageDir && ANNOUNCEMENT_VERSION) {
      try {
        mkdirSync(storageDir, { recursive: true });
        writeFileSync(join(storageDir, "last_announced_version"), ANNOUNCEMENT_VERSION);
      } catch {
        // best-effort
      }
    }
    return { success: true };
  });

  rpcServer.handle("get-warnings", async () => {
    const warnings: string[] = [];
    if (
      aftConfig.semantic_search &&
      isFastembedSemanticBackend &&
      !configOverrides._ort_dylib_dir
    ) {
      if (!isOrtAutoDownloadSupported()) {
        warnings.push(`Semantic search requires ONNX Runtime.\nInstall: ${getManualInstallHint()}`);
      }
    }
    return { warnings };
  });

  rpcServer.start().catch((err) => warn(`RPC server failed to start: ${err}`));

  // Periodic URL cache cleanup (fire-and-forget, removes entries older than 24 hours)
  try {
    cleanupUrlCache(storageDir);
  } catch {
    // best-effort
  }

  try {
    ensureTuiPluginEntry();
  } catch {
    // Best-effort only
  }

  // --- Startup notifications (fire-and-forget, best-effort) ---
  const notifyOpts: NotificationOptions = {
    client: input.client,
    directory: input.directory,
  };

  // Feature announcements in TUI are handled by the TUI plugin via RPC (get-announcement + dialog).
  // In Desktop, sendFeatureAnnouncement sends an ignored message to the active session.
  // Both share the same last_announced_version file and the same ANNOUNCEMENT_VERSION
  // constant, so bugfix releases don't re-fire a stale dialog. No-op when empty.
  if (ANNOUNCEMENT_VERSION && ANNOUNCEMENT_FEATURES.length > 0) {
    setTimeout(() => {
      sendFeatureAnnouncement(
        notifyOpts,
        ANNOUNCEMENT_VERSION,
        ANNOUNCEMENT_FEATURES,
        storageDir,
      ).catch(() => {});
    }, 8000);
  }

  // Warn about ONNX Runtime if semantic search is enabled but ORT is unavailable
  if (aftConfig.semantic_search && isFastembedSemanticBackend && !configOverrides._ort_dylib_dir) {
    // The ensureOnnxRuntime call above is async and may still be in flight.
    // Schedule the warning check after a short delay to let it resolve.
    setTimeout(() => {
      if (!configOverrides._ort_dylib_dir && !isOrtAutoDownloadSupported()) {
        sendWarning(
          notifyOpts,
          `Semantic search requires ONNX Runtime.\nInstall: ${getManualInstallHint()}`,
        ).catch(() => {});
      }
    }, 5000);
  } else {
    // No warnings needed — clean up any stale warnings from previous runs
    cleanupWarnings(notifyOpts).catch(() => {});
  }

  // Tool surface tiers:
  //   minimal:     aft_outline, aft_zoom, aft_safety
  //   recommended: minimal + hoisted + lsp_diagnostics + ast_grep_* + aft_import (default)
  //   all:         recommended + aft_navigate, aft_delete, aft_move, aft_transform, aft_refactor
  const surface = aftConfig.tool_surface ?? "recommended";

  // Tools only available in "all" tier
  const ALL_ONLY_TOOLS = new Set([
    "aft_navigate",
    "aft_delete",
    "aft_move",
    "aft_transform",
    "aft_refactor",
  ]);

  // Build full tool map
  const allTools = normalizeToolMap({
    // Hoisted tools: only in recommended+ (and when hoist_builtin_tools !== false)
    ...(surface !== "minimal" &&
      (aftConfig.hoist_builtin_tools !== false ? hoistedTools(ctx) : aftPrefixedTools(ctx))),
    ...readingTools(ctx),

    ...safetyTools(ctx),
    // aft_import: recommended+
    ...(surface !== "minimal" && importTools(ctx)),
    ...structureTools(ctx),
    ...navigationTools(ctx),
    // AST tools: recommended+
    ...(surface !== "minimal" && astTools(ctx)),
    ...(surface !== "minimal" && aftConfig.semantic_search === true && semanticTools(ctx)),
    // Indexed search tools: recommended+ and opt-in
    ...(surface !== "minimal" && aftConfig.search_index === true && searchTools(ctx)),
    ...refactoringTools(ctx),
    // LSP diagnostics: recommended+
    ...(surface !== "minimal" && lspTools(ctx)),
    // Git conflicts: recommended+
    ...(surface !== "minimal" && conflictTools(ctx)),
  });

  // Remove all-only tools when surface is minimal or recommended
  if (surface !== "all") {
    for (const name of ALL_ONLY_TOOLS) {
      if (name in allTools) {
        delete allTools[name];
      }
    }
  }

  // Filter disabled tools (user + project config union)
  const disabled = new Set(aftConfig.disabled_tools ?? []);
  if (disabled.size > 0) {
    for (const name of disabled) {
      if (name in allTools) {
        delete allTools[name];
      } else {
        warn(
          `disabled_tools: "${name}" not found — available: ${Object.keys(allTools).join(", ")}`,
        );
      }
    }
    log(`Disabled ${disabled.size} tool(s): ${[...disabled].join(", ")}`);
  }

  const autoUpdateEventHook = createAutoUpdateCheckerHook(input, {
    enabled: true,
    autoUpdate: aftConfig.auto_update ?? true,
    signal: autoUpdateAbort.signal,
  });

  return {
    tool: allTools,
    event: async (eventInput: { event: { type: string; properties?: unknown } }) => {
      await autoUpdateEventHook(eventInput);
      if (eventInput.event.type !== "session.idle") return;
      const sessionID = extractSessionID(eventInput.event.properties);
      if (!sessionID) return;
      await handleIdleBgCompletions({
        ctx,
        directory: input.directory,
        sessionID,
        client: input.client,
      });
    },
    "chat.message": async (messageInput: {
      sessionID?: string;
      sessionId?: string;
      id?: string;
    }) => {
      resetBgWake(messageInput.sessionID ?? messageInput.sessionId ?? messageInput.id);
    },
    "command.execute.before": async (
      commandInput: { command: string; sessionID: string },
      _output: unknown,
    ) => {
      if (isTuiMode() || commandInput.command !== STATUS_COMMAND) {
        return;
      }

      // Prefer an existing active bridge to get warm index status
      const bridge =
        ctx.pool.getAnyActiveBridge(input.directory) ?? ctx.pool.getBridge(input.directory);
      const response = await bridge.send("status", { session_id: commandInput.sessionID });
      if (response.success === false) {
        throw new Error((response.message as string) || "status failed");
      }

      const status = coerceAftStatus(response);
      await sendIgnoredMessage(input.client, commandInput.sessionID, formatStatusMarkdown(status));
      throwSentinel(commandInput.command);
    },
    // Restore metadata that fromPlugin() overwrites (opencode bug workaround)
    "tool.execute.after": async (
      toolInput: { tool: string; sessionID: string; callID: string },
      output: { title: string; output: string; metadata: Record<string, unknown> } | undefined,
    ) => {
      if (!output) return;
      const stored = consumeToolMetadata(toolInput.sessionID, toolInput.callID);
      if (stored) {
        if (stored.title) output.title = stored.title;
        if (stored.metadata) output.metadata = { ...output.metadata, ...stored.metadata };
      }
      // Hint: when a git merge/rebase produces conflicts, nudge the agent toward aft_conflicts
      if (
        toolInput.tool === "bash" &&
        output.output?.includes("Automatic merge failed; fix conflicts")
      ) {
        output.output +=
          "\n\n[Hint] Use aft_conflicts to see all conflict regions across files in a single call.";
      }
      // Hint: when agent runs grep/rg via bash, nudge toward the built-in grep tool.
      // Detection: check the first line of output (the echoed command) for rg or grep invocations.
      if (toolInput.tool === "bash" && output.output) {
        const firstLine = output.output.slice(0, 300).split("\n")[0] ?? "";
        if (/\b(rg|grep)\s/.test(firstLine)) {
          output.output +=
            "\n\n[Hint] Use the grep tool instead of bash for faster indexed search.";
        }
      }
      await appendInTurnBgCompletions(
        { ctx, directory: input.directory, sessionID: toolInput.sessionID },
        output,
      );
    },
    config: async (config) => {
      // Register /aft-status for Desktop command palette.
      // In TUI mode, the TUI plugin also registers it via api.command.register()
      // which takes priority for dialog rendering.
      config.command = {
        ...(config.command ?? {}),
        [STATUS_COMMAND]: {
          template: STATUS_COMMAND,
          description: "Show AFT status, index health, cache usage, and runtime details",
        },
      };
    },
    dispose: async () => {
      autoUpdateAbort.abort();
      unregisterShutdown();
      await Promise.allSettled([abortInFlightAutoInstalls(), abortInFlightGithubInstalls()]);
      rpcServer.stop();
      clearSharedBridgePool();
      await pool.shutdown();
    },
  };
};

export default plugin;
