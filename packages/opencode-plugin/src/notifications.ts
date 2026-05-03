/**
 * User-visible notifications for AFT plugin.
 *
 * Two delivery paths based on the OpenCode client:
 *   - Desktop: Sends ignored messages to the active session (appears in chat, hidden from LLM)
 *   - TUI: Uses client.tui.showToast() for transient toast notifications
 *
 * Use cases:
 *   - Feature announcements (new version, new experimental features)
 *   - Warnings (ONNX Runtime not found, stale binary)
 *   - Status updates (semantic search ready, index built)
 *
 * Messages are identified by markers and cleaned up on subsequent startups
 * when no longer relevant (Desktop only — TUI toasts are inherently transient).
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { join } from "node:path";
import { sessionLog } from "./logger.js";

// --- TUI toast helper ---

type TuiClient = {
  tui?: {
    showToast?: (input: {
      body: {
        title: string;
        message: string;
        variant?: "info" | "warning" | "error" | "success";
        duration?: number;
      };
    }) => Promise<unknown>;
  };
};

function isTuiMode(): boolean {
  return process.env.OPENCODE_CLIENT === "cli";
}

async function showTuiToast(
  client: unknown,
  title: string,
  message: string,
  variant: "info" | "warning" | "error" | "success" = "info",
  duration = 8000,
): Promise<boolean> {
  const c = client as TuiClient;
  if (typeof c.tui?.showToast !== "function") return false;
  try {
    await c.tui.showToast({ body: { title, message, variant, duration } });
    return true;
  } catch {
    return false;
  }
}

// --- Markers for message identification ---

/** Prefix for all AFT notification messages */
const AFT_MARKER = "🔧 AFT:";

/** Marker for feature announcements */
const FEATURE_MARKER = `${AFT_MARKER} New in`;

/** Marker for warnings (ONNX missing, etc.) */
const WARNING_MARKER = `${AFT_MARKER} ⚠️`;

/** Marker for transient status updates */
const STATUS_MARKER = `${AFT_MARKER} ✅`;

const WARNED_TOOLS_FILE = "warned_tools.json";

// --- Desktop state file resolution ---

function getDesktopStatePath(): string | null {
  const os = platform();
  const home = homedir();

  if (os === "darwin") {
    return join(
      home,
      "Library",
      "Application Support",
      "ai.opencode.desktop",
      "opencode.global.dat",
    );
  }
  if (os === "linux") {
    const xdgConfig = process.env.XDG_CONFIG_HOME || join(home, ".config");
    return join(xdgConfig, "ai.opencode.desktop", "opencode.global.dat");
  }
  if (os === "win32") {
    const appData = process.env.APPDATA || join(home, "AppData", "Roaming");
    return join(appData, "ai.opencode.desktop", "opencode.global.dat");
  }

  return null;
}

interface DesktopState {
  sessionId: string | null;
  serverUrl: string | null;
}

function readDesktopState(directory: string): DesktopState {
  const statePath = getDesktopStatePath();
  if (!statePath || !existsSync(statePath)) {
    return { sessionId: null, serverUrl: null };
  }

  try {
    const raw = readFileSync(statePath, "utf-8");
    const state = JSON.parse(raw) as Record<string, unknown>;

    // Extract sidecar URL from server state
    let serverUrl: string | null = null;
    const serverStr = state.server;
    if (typeof serverStr === "string") {
      try {
        const serverState = JSON.parse(serverStr) as Record<string, unknown>;
        if (typeof serverState.currentSidecarUrl === "string") {
          serverUrl = serverState.currentSidecarUrl;
        }
      } catch {
        // ignore
      }
    }

    // Extract last session for directory
    let sessionId: string | null = null;
    const layoutPage = state["layout.page"];
    if (typeof layoutPage === "string") {
      const parsed = JSON.parse(layoutPage) as Record<string, unknown>;
      const lastProjectSession = parsed.lastProjectSession as
        | Record<string, { id?: string }>
        | undefined;
      if (lastProjectSession) {
        const entry = lastProjectSession[directory];
        sessionId = entry?.id ?? null;
      }
    }

    return { sessionId, serverUrl };
  } catch {
    return { sessionId: null, serverUrl: null };
  }
}

// --- SDK message helpers ---

type SdkMessage = {
  info?: { id?: string; role?: string };
  parts?: Array<{ type?: string; text?: string; ignored?: boolean }>;
};

function getServerAuth(): string | undefined {
  const password = process.env.OPENCODE_SERVER_PASSWORD;
  if (!password) return undefined;
  const username = process.env.OPENCODE_SERVER_USERNAME ?? "opencode";
  return `Basic ${Buffer.from(`${username}:${password}`, "utf8").toString("base64")}`;
}

async function getSessionMessages(client: unknown, sessionId: string): Promise<SdkMessage[]> {
  try {
    const c = client as {
      session?: {
        messages?: (input: { path: { id: string } }) => Promise<{ data?: SdkMessage[] }>;
      };
    };
    if (typeof c.session?.messages === "function") {
      const result = await c.session.messages({ path: { id: sessionId } });
      return result?.data ?? [];
    }
  } catch {
    // silent
  }
  return [];
}

async function sendIgnoredMessage(
  client: unknown,
  sessionId: string,
  text: string,
): Promise<boolean> {
  try {
    const c = client as {
      session?: {
        prompt?: (input: unknown) => unknown;
        promptAsync?: (input: unknown) => unknown;
      };
    };

    // `noReply: true` means OpenCode appends this as a synthetic user
    // message and does NOT trigger an assistant turn. No LLM call
    // happens, so model/variant/agent passthrough is unnecessary here.
    // Keeping the body minimal also avoids OpenCode-side crashes that
    // surfaced when we passed model/agent on this path. Cache-preserving
    // model/variant forwarding belongs ONLY on wake-style calls
    // (noReply: false), which live in bg-notifications.ts.
    const promptInput = {
      path: { id: sessionId },
      body: {
        noReply: true,
        parts: [{ type: "text", text, ignored: true }],
      },
    };

    if (typeof c.session?.prompt === "function") {
      await Promise.resolve(c.session.prompt(promptInput));
      return true;
    }
    if (typeof c.session?.promptAsync === "function") {
      await c.session.promptAsync(promptInput);
      return true;
    }
  } catch (err) {
    sessionLog(
      sessionId,
      `[aft-plugin] notification send failed: ${err instanceof Error ? err.message : String(err)}`,
    );
  }
  return false;
}

async function deleteMessage(
  serverUrl: string,
  sessionId: string,
  messageId: string,
): Promise<boolean> {
  const auth = getServerAuth();
  const url = `${serverUrl}/session/${encodeURIComponent(sessionId)}/message/${encodeURIComponent(messageId)}`;

  try {
    const response = await fetch(url, {
      method: "DELETE",
      headers: auth ? { Authorization: auth } : {},
      signal: AbortSignal.timeout(10_000),
    });
    return response.ok;
  } catch {
    return false;
  }
}

// --- Public API ---

export interface NotificationOptions {
  /** The OpenCode SDK client */
  client: unknown;
  /** Project directory for Desktop state lookup */
  directory: string;
  /** Server URL for message deletion (optional — from ctx.serverUrl) */
  serverUrl?: string;
}

export interface ConfigureWarning {
  kind: "formatter_not_installed" | "checker_not_installed" | "lsp_binary_missing";
  language?: string;
  server?: string;
  tool?: string;
  binary?: string;
  hint: string;
}

export interface ConfigureWarningOptions {
  client: unknown;
  sessionId: string;
  storageDir: string;
  pluginVersion: string;
  projectRoot?: string;
}

/**
 * Send a persistent warning notification.
 * Desktop: ignored message, cleaned up on next startup when resolved.
 * TUI: toast with warning variant (inherently transient).
 */
export async function sendWarning(opts: NotificationOptions, message: string): Promise<void> {
  // Try TUI toast first, fall back to Desktop ignored message
  const toastSent = await showTuiToast(opts.client, "AFT Warning", message, "warning", 10000);
  if (toastSent) return;

  const { sessionId } = readDesktopState(opts.directory);
  if (!sessionId) return;

  const text = `${WARNING_MARKER} ${message}`;
  sessionLog(sessionId, `[aft-plugin] sending warning to session ${sessionId}`);
  await sendIgnoredMessage(opts.client, sessionId, text);
}

/**
 * Send a transient status notification.
 * Desktop: ignored message, auto-deletes after 3 seconds.
 * TUI: toast with success variant, auto-dismissed by the TUI.
 */
export async function sendStatus(opts: NotificationOptions, message: string): Promise<void> {
  if (isTuiMode()) {
    await showTuiToast(opts.client, "AFT", message, "success", 3000);
    return;
  }

  const { sessionId, serverUrl: desktopServerUrl } = readDesktopState(opts.directory);
  if (!sessionId) return;

  const text = `${STATUS_MARKER} ${message}`;
  await sendIgnoredMessage(opts.client, sessionId, text);

  // Auto-delete after 3 seconds
  const effectiveServerUrl = opts.serverUrl || desktopServerUrl;
  if (!effectiveServerUrl) return;

  setTimeout(async () => {
    try {
      const msgs = await getSessionMessages(opts.client, sessionId);
      for (let i = msgs.length - 1; i >= 0; i--) {
        const msg = msgs[i];
        const msgId = msg.info?.id;
        if (!msgId || msg.info?.role !== "user") break;
        const isOurs =
          msg.parts?.length &&
          msg.parts.every(
            (p) =>
              p.ignored === true &&
              p.type === "text" &&
              typeof p.text === "string" &&
              p.text.startsWith(STATUS_MARKER),
          );
        if (isOurs) {
          await deleteMessage(effectiveServerUrl, sessionId, msgId);
        } else {
          break;
        }
      }
    } catch {
      // best-effort
    }
  }, 3000);
}

/**
 * Send a feature announcement for a new version.
 * Tracked via a version file in storageDir — only fires once per version across all sessions.
 * Desktop: ignored message in the active session.
 * TUI: toast with info variant.
 */
export async function sendFeatureAnnouncement(
  opts: NotificationOptions,
  version: string,
  features: string[],
  storageDir?: string,
): Promise<void> {
  // Check if we already announced this version (persisted across sessions)
  if (storageDir) {
    const versionFile = join(storageDir, "last_announced_version");
    try {
      if (existsSync(versionFile)) {
        const lastVersion = readFileSync(versionFile, "utf-8").trim();
        if (lastVersion === version) return;
      }
    } catch {
      // ignore read errors — proceed with announcement
    }
  }

  const featureText = features.map((f) => `• ${f}`).join("\n");

  // Try TUI toast first (works when client exposes tui.showToast),
  // fall back to Desktop ignored message
  const toastSent = await showTuiToast(opts.client, `AFT v${version}`, featureText, "info", 12000);
  if (!toastSent) {
    const { sessionId } = readDesktopState(opts.directory);
    if (!sessionId) return;

    const text = [`${FEATURE_MARKER} v${version}:`, ...features.map((f) => `  • ${f}`)].join("\n");
    sessionLog(sessionId, `[aft-plugin] sending feature announcement for v${version}`);
    await sendIgnoredMessage(opts.client, sessionId, text);
  }

  // Persist the announced version
  if (storageDir) {
    try {
      mkdirSync(storageDir, { recursive: true });
      writeFileSync(join(storageDir, "last_announced_version"), version);
    } catch {
      // best-effort
    }
  }
}

function readWarnedTools(storageDir: string): Record<string, string> {
  try {
    const warnedToolsPath = join(storageDir, WARNED_TOOLS_FILE);
    if (!existsSync(warnedToolsPath)) return {};

    const parsed = JSON.parse(readFileSync(warnedToolsPath, "utf-8")) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};

    const warned: Record<string, string> = {};
    for (const [key, version] of Object.entries(parsed)) {
      if (typeof version === "string") {
        warned[key] = version;
      }
    }
    return warned;
  } catch {
    return {};
  }
}

function writeWarnedTools(storageDir: string, warned: Record<string, string>): void {
  try {
    mkdirSync(storageDir, { recursive: true });
    const warnedToolsPath = join(storageDir, WARNED_TOOLS_FILE);
    // Direct write — this state is best-effort and rapid sequential calls
    // (e.g. inside tests) hit Date.now() collisions on fast runners, making
    // a temp+rename strategy no safer than a plain write here.
    writeFileSync(warnedToolsPath, `${JSON.stringify(warned, null, 2)}\n`);
  } catch {
    // best-effort
  }
}

function warningKey(warning: ConfigureWarning, projectRoot?: string): string {
  const scope = warning.kind === "lsp_binary_missing" ? "_" : (projectRoot ?? "_");
  return [
    scope,
    warning.kind,
    warning.language ?? warning.server ?? "_",
    warning.tool ?? warning.binary ?? "_",
    warning.hint,
  ]
    .map((part) => encodeURIComponent(part))
    .join(":");
}

function warningTitle(warning: ConfigureWarning): string {
  switch (warning.kind) {
    case "formatter_not_installed":
      return "Formatter is not installed";
    case "checker_not_installed":
      return "Checker is not installed";
    case "lsp_binary_missing":
      return "LSP binary is missing";
  }
}

function formatConfigureWarning(warning: ConfigureWarning): string {
  const details: string[] = [];
  if (warning.language) details.push(`language: ${warning.language}`);
  if (warning.server) details.push(`server: ${warning.server}`);
  if (warning.tool) details.push(`tool: ${warning.tool}`);
  if (warning.binary && warning.binary !== warning.tool) {
    details.push(`binary: ${warning.binary}`);
  }

  const suffix = details.length > 0 ? ` (${details.join(", ")})` : "";
  return `${WARNING_MARKER} ${warningTitle(warning)}${suffix}\n${warning.hint}`;
}

export async function deliverConfigureWarnings(
  opts: ConfigureWarningOptions,
  warnings: ConfigureWarning[],
): Promise<void> {
  if (warnings.length === 0) return;

  const warned = readWarnedTools(opts.storageDir);
  let changed = false;

  for (const warning of warnings) {
    const key = warningKey(warning, opts.projectRoot);
    if (Object.hasOwn(warned, key)) continue;

    const delivered = await sendIgnoredMessage(
      opts.client,
      opts.sessionId,
      formatConfigureWarning(warning),
    );
    if (!delivered) continue;

    warned[key] = opts.pluginVersion;
    changed = true;
  }

  if (changed) {
    writeWarnedTools(opts.storageDir, warned);
  }
}

/**
 * Clean up stale AFT warning messages from previous runs.
 * Desktop only — TUI toasts are inherently transient and don't need cleanup.
 */
export async function cleanupWarnings(opts: NotificationOptions): Promise<void> {
  if (isTuiMode()) return; // TUI toasts don't persist

  const { sessionId, serverUrl: desktopServerUrl } = readDesktopState(opts.directory);
  if (!sessionId) return;

  const effectiveServerUrl = opts.serverUrl || desktopServerUrl;
  if (!effectiveServerUrl) return;

  const messages = await getSessionMessages(opts.client, sessionId);
  if (messages.length === 0) return;

  // Scan from end for consecutive AFT warning messages
  const warningIds: string[] = [];
  for (let i = messages.length - 1; i >= 0; i--) {
    const msg = messages[i];
    const msgId = msg.info?.id;
    if (!msgId || msg.info?.role !== "user") break;

    const isAftWarning =
      msg.parts?.length &&
      msg.parts.every(
        (p) =>
          p.ignored === true &&
          p.type === "text" &&
          typeof p.text === "string" &&
          p.text.startsWith(WARNING_MARKER),
      );

    if (isAftWarning) {
      warningIds.push(msgId);
    } else {
      break;
    }
  }

  if (warningIds.length === 0) return;

  sessionLog(sessionId, `[aft-plugin] cleaning up ${warningIds.length} stale warning(s)`);
  for (const id of warningIds) {
    await deleteMessage(effectiveServerUrl, sessionId, id);
  }
}
