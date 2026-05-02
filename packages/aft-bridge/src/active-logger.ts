/**
 * Internal helper that lets shared modules call simple `log/warn/error`
 * functions without threading a {@link Logger} through every signature.
 *
 * The host (OpenCode plugin, Pi plugin) calls {@link setActiveLogger} once at
 * startup before constructing any {@link BridgePool}. Internal callers use
 * {@link log}/{@link warn}/{@link error} which forward to the active logger.
 *
 * If no logger has been set, calls fall back to `console.error` so we never
 * silently drop diagnostics.
 */
import type { Logger, LogMeta } from "./logger.js";

let active: Logger | undefined;

export function setActiveLogger(logger: Logger): void {
  active = logger;
}

export function getActiveLogger(): Logger | undefined {
  return active;
}

export function getLogFilePath(): string | undefined {
  return active?.getLogFilePath?.();
}

export function log(message: string, meta?: LogMeta): void {
  if (active) {
    active.log(message, meta);
  } else {
    console.error(`[aft-bridge] ${message}`);
  }
}

export function warn(message: string, meta?: LogMeta): void {
  if (active) {
    active.warn(message, meta);
  } else {
    console.error(`[aft-bridge] WARN: ${message}`);
  }
}

export function error(message: string, meta?: LogMeta): void {
  if (active) {
    active.error(message, meta);
  } else {
    console.error(`[aft-bridge] ERROR: ${message}`);
  }
}

export function sessionLog(sessionId: string | undefined, message: string): void {
  log(message, sessionId ? { sessionId } : undefined);
}

export function sessionWarn(sessionId: string | undefined, message: string): void {
  warn(message, sessionId ? { sessionId } : undefined);
}

export function sessionError(sessionId: string | undefined, message: string): void {
  error(message, sessionId ? { sessionId } : undefined);
}
