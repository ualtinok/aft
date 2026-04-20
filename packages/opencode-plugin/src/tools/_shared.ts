/**
 * Shared helpers for plugin tool handlers.
 *
 * Every tool that talks to the Rust binary should use `callBridge()` instead
 * of calling `ctx.pool.getBridge(...).send(...)` directly. The helper:
 *
 *   1. Resolves the project root from `context.worktree ?? context.directory`
 *      (canonical path), so two tool calls in the same project always reach
 *      the same bridge even if the agent's cwd momentarily differs.
 *   2. Injects `session_id` from `context.sessionID` into every request so the
 *      Rust side can partition undo/checkpoint state per OpenCode session
 *      (issue #14 — one shared bridge per project, N sessions per bridge).
 *
 * Tools that specifically need the raw `BinaryBridge` (for example to call
 * `bridge.send()` multiple times with shared state) should use `bridgeFor()`
 * and still pass `session_id` explicitly.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import type { BinaryBridge } from "../bridge.js";
import type { PluginContext } from "../types.js";

/**
 * Minimum shape of the per-tool-call context provided by the OpenCode SDK.
 *
 * We only depend on a few fields so any similar context (including the Pi
 * plugin's `ExtensionContext`) can be passed through the same helpers once
 * they adopt session-aware calls.
 */
export interface ToolRuntime {
  /** Worktree root (preferred); falls back to `directory` when absent. */
  worktree?: string;
  /** Agent's working directory for this tool call. */
  directory: string;
  /** Opaque OpenCode session identifier. Missing in CLI tests / some hosts. */
  sessionID?: string;
}

/**
 * Resolve the canonical project root for a runtime.
 *
 * Prefers `worktree` because that stays stable across OpenCode sessions in
 * the same project; falls back to `directory` when unavailable (standalone
 * CLI use, older hosts). Normalizes trailing slashes and resolves symlinks
 * via `realpath` so `/repo` and `/repo/` and `/Users/.../repo -> /Volumes/...`
 * collapse to the same key.
 */
export function projectRootFor(runtime: ToolRuntime): string {
  const raw = runtime.worktree ?? runtime.directory;
  // Strip trailing separators first so realpath failure still produces a
  // consistent fallback key.
  const trimmed = raw.replace(/[/\\]+$/, "");
  try {
    return fs.realpathSync(trimmed);
  } catch {
    // realpath fails for paths that don't exist (e.g. test fixtures created
    // lazily). Fall back to lexical resolution so we still produce a stable
    // key rather than crashing.
    return path.resolve(trimmed);
  }
}

/**
 * Get the BinaryBridge for the runtime's project root.
 *
 * Prefer `callBridge()` unless you need to send multiple requests yourself.
 */
export function bridgeFor(ctx: PluginContext, runtime: ToolRuntime): BinaryBridge {
  return ctx.pool.getBridge(projectRootFor(runtime));
}

/**
 * Send a single command to the Rust binary with `session_id` injected.
 *
 * This is the canonical way for a tool handler to call AFT: the helper picks
 * the right bridge (project-keyed), attaches the session namespace from
 * `context.sessionID`, and returns whatever the binary responds.
 *
 * The Rust side falls back to a shared default namespace when `session_id`
 * is absent (see `RawRequest::session()`), so hosts that don't expose a
 * session identifier still work — they just share undo/checkpoint state.
 */
export function callBridge(
  ctx: PluginContext,
  runtime: ToolRuntime,
  command: string,
  params: Record<string, unknown> = {},
): ReturnType<BinaryBridge["send"]> {
  const merged: Record<string, unknown> = { ...params };
  if (runtime.sessionID) {
    merged.session_id = runtime.sessionID;
  }
  return bridgeFor(ctx, runtime).send(command, merged);
}
