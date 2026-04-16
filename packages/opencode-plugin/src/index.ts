import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import type { Plugin } from "@opencode-ai/plugin";

import { loadAftConfig } from "./config.js";
import { ensureBinary } from "./downloader.js";
import { error, log, warn } from "./logger.js";
import { consumeToolMetadata } from "./metadata-store.js";
import { normalizeToolMap } from "./normalize-schemas.js";
import {
  cleanupWarnings,
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
 * AFT (Agent File Toolkit) plugin for OpenCode.
 *
 * Config is loaded from two levels (project overrides user):
 * - User:    ~/.config/opencode/aft.jsonc (or .json)
 * - Project: <project>/.opencode/aft.jsonc (or .json)
 *
 * Tools organized into groups:
 * - Hoisted (default): read, write, edit, apply_patch, ast_grep_search, ast_grep_replace
 *   and experimental grep/glob when experimental_search_index is enabled
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

  // Build config overrides for the Rust binary (strip undefined values)
  const configOverrides: Record<string, unknown> = {};
  if (aftConfig.format_on_edit !== undefined)
    configOverrides.format_on_edit = aftConfig.format_on_edit;
  if (aftConfig.validate_on_edit !== undefined)
    configOverrides.validate_on_edit = aftConfig.validate_on_edit;
  if (aftConfig.formatter !== undefined) configOverrides.formatter = aftConfig.formatter;
  if (aftConfig.checker !== undefined) configOverrides.checker = aftConfig.checker;
  if (aftConfig.restrict_to_project_root !== undefined)
    configOverrides.restrict_to_project_root = aftConfig.restrict_to_project_root;
  if (aftConfig.experimental_search_index !== undefined)
    configOverrides.experimental_search_index = aftConfig.experimental_search_index;
  if (aftConfig.experimental_semantic_search !== undefined)
    configOverrides.experimental_semantic_search = aftConfig.experimental_semantic_search;
  if (aftConfig.semantic !== undefined) configOverrides.semantic = aftConfig.semantic;

  const isFastembedSemanticBackend = (aftConfig.semantic?.backend ?? "fastembed") === "fastembed";

  // Compute XDG-compliant storage dir for persistent indexes (trigram, semantic)
  // Pattern: ~/.local/share/opencode/storage/plugin/aft/
  const dataHome = process.env.XDG_DATA_HOME || join(homedir(), ".local", "share");
  configOverrides.storage_dir = join(dataHome, "opencode", "storage", "plugin", "aft");

  // Auto-resolve ONNX Runtime for semantic search.
  // Downloads the shared library on first use if the platform is supported.
  // The resolved path is passed to bridges via ORT_DYLIB_PATH env var.
  if (aftConfig.experimental_semantic_search && isFastembedSemanticBackend) {
    const storageDir = configOverrides.storage_dir as string;
    ensureOnnxRuntime(storageDir).then(
      (ortDir) => {
        if (ortDir) {
          configOverrides._ort_dylib_dir = ortDir;
        } else if (!isOrtAutoDownloadSupported()) {
          warn(`Semantic search requires ONNX Runtime. Install: ${getManualInstallHint()}`);
        }
      },
      (err) => warn(`ONNX Runtime resolution failed: ${err}`),
    );
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
    },
    configOverrides,
  );
  const ctx: PluginContext = {
    pool,
    client: input.client,
    config: aftConfig,
    storageDir: configOverrides.storage_dir as string,
  };
  setSharedBridgePool(pool);

  // Start RPC server for TUI plugin communication
  const rpcServer = new AftRpcServer(configOverrides.storage_dir as string, input.directory);
  rpcServer.handle("status", async (params) => {
    const sessionID = (params.sessionID as string) || "rpc";
    const bridge =
      pool.getAnyActiveBridge(input.directory) ?? pool.getBridge(input.directory, sessionID);
    return await bridge.send("status", {});
  });
  // Feature announcement data — TUI plugin calls this on startup to show dialog
  const storageDir = configOverrides.storage_dir as string;
  const featureList = [
    "Semantic code search (`aft_search`) — enable with `experimental_semantic_search: true` in aft.jsonc",
    "/aft-status command — live index health, disk usage, and runtime details",
    "HTML outline and zoom — heading hierarchy for .html/.htm files",
    "And many bugfixes",
  ];

  rpcServer.handle("get-announcement", async () => {
    // Check if already announced this version
    if (storageDir) {
      const versionFile = join(storageDir, "last_announced_version");
      try {
        if (existsSync(versionFile)) {
          const lastVersion = readFileSync(versionFile, "utf-8").trim();
          if (lastVersion === PLUGIN_VERSION) return { show: false };
        }
      } catch {
        // proceed
      }
    }
    return { show: true, version: PLUGIN_VERSION, features: featureList };
  });

  rpcServer.handle("mark-announced", async () => {
    if (storageDir) {
      try {
        mkdirSync(storageDir, { recursive: true });
        writeFileSync(join(storageDir, "last_announced_version"), PLUGIN_VERSION);
      } catch {
        // best-effort
      }
    }
    return { success: true };
  });

  rpcServer.handle("get-warnings", async () => {
    const warnings: string[] = [];
    if (
      aftConfig.experimental_semantic_search &&
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
  // Both share the same last_announced_version file — TUI checks first via RPC,
  // Desktop fires with a delay to avoid racing the TUI path.
  setTimeout(() => {
    sendFeatureAnnouncement(notifyOpts, PLUGIN_VERSION, featureList, storageDir).catch(() => {});
  }, 8000);

  // Warn about ONNX Runtime if semantic search is enabled but ORT is unavailable
  if (
    aftConfig.experimental_semantic_search &&
    isFastembedSemanticBackend &&
    !configOverrides._ort_dylib_dir
  ) {
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
    ...(surface !== "minimal" &&
      aftConfig.experimental_semantic_search === true &&
      semanticTools(ctx)),
    // Indexed search tools: recommended+ and opt-in
    ...(surface !== "minimal" && aftConfig.experimental_search_index === true && searchTools(ctx)),
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

  return {
    tool: allTools,
    "command.execute.before": async (
      commandInput: { command: string; sessionID: string },
      _output: unknown,
    ) => {
      if (isTuiMode() || commandInput.command !== STATUS_COMMAND) {
        return;
      }

      // Prefer an existing active bridge to get warm index status
      const bridge =
        ctx.pool.getAnyActiveBridge(input.directory) ??
        ctx.pool.getBridge(input.directory, commandInput.sessionID);
      const response = await bridge.send("status", {});
      if (response.success === false) {
        throw new Error((response.message as string) || "status failed");
      }

      const status = coerceAftStatus(response);
      await sendIgnoredMessage(input.client, commandInput.sessionID, formatStatusMarkdown(status));
      throwSentinel(commandInput.command);
    },
    // Restore metadata that fromPlugin() overwrites (opencode bug workaround)
    "tool.execute.after": async (
      input: { tool: string; sessionID: string; callID: string },
      output: { title: string; output: string; metadata: Record<string, unknown> } | undefined,
    ) => {
      if (!output) return;
      const stored = consumeToolMetadata(input.sessionID, input.callID);
      if (stored) {
        if (stored.title) output.title = stored.title;
        if (stored.metadata) output.metadata = { ...output.metadata, ...stored.metadata };
      }
      // Hint: when a git merge/rebase produces conflicts, nudge the agent toward aft_conflicts
      if (
        input.tool === "bash" &&
        output.output?.includes("Automatic merge failed; fix conflicts")
      ) {
        output.output +=
          "\n\n[Hint] Use aft_conflicts to see all conflict regions across files in a single call.";
      }
      // Hint: when agent runs grep/rg via bash, nudge toward the built-in grep tool.
      // Detection: check the first line of output (the echoed command) for rg or grep invocations.
      if (input.tool === "bash" && output.output) {
        const firstLine = output.output.slice(0, 300).split("\n")[0] ?? "";
        if (/\b(rg|grep)\s/.test(firstLine)) {
          output.output +=
            "\n\n[Hint] Use the grep tool instead of bash for faster indexed search.";
        }
      }
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
    dispose: () => {
      rpcServer.stop();
      clearSharedBridgePool();
      return pool.shutdown();
    },
  };
};

export default plugin;
