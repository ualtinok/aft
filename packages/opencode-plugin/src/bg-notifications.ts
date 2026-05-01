import { sessionWarn } from "./logger.js";
import { getLastUserModel } from "./shared/last-user-model.js";
import type { PluginContext } from "./types.js";

export interface BgCompletion {
  task_id: string;
  status: string;
  exit_code: number | null;
  command: string;
  duration_ms?: number;
  runtime_ms?: number;
  runtime?: number;
  /** Tail of stdout+stderr captured at completion (≤300 bytes from Rust). */
  output_preview?: string;
  /** True when the captured tail is shorter than the actual output. */
  output_truncated?: boolean;
}

type SessionBgState = {
  outstandingTaskIds: Set<string>;
  pendingCompletions: BgCompletion[];
  debounceTimer: NodeJS.Timeout | null;
  wakeFiredThisIdle: boolean;
  firstCompletionAt: number | null;
  scheduledFireAt: number | null;
  scheduledCompletionCount: number;
  retryDelayMs: number | null;
  unknownCompletions: Array<{ completion: BgCompletion; receivedAt: number }>;
  lastSeenAt: number;
};

export const sessionBgStates: Map<string, SessionBgState> = new Map();

// Lazily evict idle, task-free sessions after 1 hour; no timer is used so the plugin doesn't keep the event loop alive.
export const SESSION_BG_STATE_IDLE_TTL_MS = 60 * 60 * 1000;
const DEBOUNCE_STEP_MS = 200;
const DEBOUNCE_CAP_MS = 1000;
const UNKNOWN_COMPLETION_TTL_MS = 5000;
const UNKNOWN_COMPLETION_CAP = 32;
const DEFAULT_SESSION_ID = "__default__";
const LOG_PREFIX = "[aft-plugin] bg-notifications:";

interface DrainContext {
  ctx: PluginContext;
  directory: string;
  sessionID: string;
  isActive?: () => boolean;
}

interface OpenCodeClient {
  session?: {
    promptAsync?: (input: unknown) => Promise<unknown> | unknown;
    messages?: (input: { path: { id: string } }) => Promise<{ data?: unknown[] }>;
  };
}

export function trackBgTask(sessionID: string | undefined, taskId: string): void {
  const state = stateFor(sessionID);
  pruneUnknownCompletions(state, Date.now());
  const buffered = state.unknownCompletions.filter((entry) => entry.completion.task_id === taskId);
  state.unknownCompletions = state.unknownCompletions.filter(
    (entry) => entry.completion.task_id !== taskId,
  );
  if (buffered.length > 0) {
    for (const entry of buffered) {
      if (!state.pendingCompletions.some((pending) => pending.task_id === taskId)) {
        state.pendingCompletions.push(entry.completion);
      }
    }
    return;
  }
  state.outstandingTaskIds.add(taskId);
}

export function ingestBgCompletions(
  sessionID: string | undefined,
  completions: unknown,
): BgCompletion[] {
  if (!Array.isArray(completions) || completions.length === 0) return [];
  const state = stateFor(sessionID);
  const accepted: BgCompletion[] = [];
  for (const completion of completions) {
    if (!isBgCompletion(completion)) continue;
    if (!state.outstandingTaskIds.has(completion.task_id)) {
      bufferUnknownCompletion(state, completion);
      continue;
    }
    state.outstandingTaskIds.delete(completion.task_id);
    if (
      !state.pendingCompletions.some((pending) => pending.task_id === completion.task_id) &&
      !accepted.some((pending) => pending.task_id === completion.task_id)
    ) {
      accepted.push(completion);
    }
  }
  state.pendingCompletions.push(...accepted);
  return accepted;
}

export async function handlePushedBgCompletion(
  drainContext: DrainContext & { client: unknown },
  completion: unknown,
): Promise<void> {
  ingestBgCompletions(drainContext.sessionID, [completion]);
  await triggerWakeIfPending(drainContext, true);
}

export async function appendInTurnBgCompletions(
  drainContext: DrainContext,
  output: { output?: string } | undefined,
): Promise<void> {
  if (!output) return;
  const state = stateFor(drainContext.sessionID);
  if (state.outstandingTaskIds.size === 0 && state.pendingCompletions.length === 0) return;

  if (state.outstandingTaskIds.size > 0) {
    await drainCompletions(drainContext);
  }
  if (state.pendingCompletions.length === 0) return;

  const reminder = formatSystemReminder(state.pendingCompletions);
  output.output = appendReminder(output.output ?? "", reminder);
  state.pendingCompletions = [];
}

export async function handleIdleBgCompletions(
  drainContext: DrainContext & { client: unknown },
): Promise<void> {
  await triggerWakeIfPending(drainContext, false);
}

async function triggerWakeIfPending(
  drainContext: DrainContext & { client: unknown },
  skipDrain: boolean,
): Promise<void> {
  const state = stateFor(drainContext.sessionID);
  if (state.wakeFiredThisIdle) return;
  if (drainContext.isActive?.()) return;

  if (!skipDrain && state.outstandingTaskIds.size > 0) {
    await drainCompletions(drainContext);
  }
  if (state.pendingCompletions.length === 0) return;

  scheduleWake(
    state,
    async (reminder) => {
      const client = drainContext.client as OpenCodeClient;
      if (typeof client.session?.promptAsync !== "function") {
        throw new Error("client.session.promptAsync is unavailable");
      }
      // Pass the last user message's model + variant explicitly so OpenCode's
      // createUserMessage doesn't fall back to agent.variant or undefined and
      // bust the provider prefix cache for the next assistant turn.
      const lastModel = await getLastUserModel(client, drainContext.sessionID);
      const body: Record<string, unknown> = {
        noReply: false,
        parts: [{ type: "text", text: reminder }],
      };
      if (lastModel) {
        body.model = { providerID: lastModel.providerID, modelID: lastModel.modelID };
        if (lastModel.variant) body.variant = lastModel.variant;
      }
      await client.session.promptAsync({
        path: { id: drainContext.sessionID },
        body,
      });
    },
    (err) => {
      sessionWarn(
        drainContext.sessionID,
        `${LOG_PREFIX} wake send failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    },
  );
}

export function resetBgWake(sessionID: string | undefined): void {
  stateFor(sessionID).wakeFiredThisIdle = false;
}

export function formatSystemReminder(completions: readonly BgCompletion[]): string {
  const bullets = completions.map((completion) => formatCompletion(completion)).join("\n");
  // Only point at bash_status when at least one completion is truncated;
  // for fully-captured short outputs the agent already has the full result.
  const anyTruncated = completions.some((c) => c.output_truncated === true);
  const tail = anyTruncated
    ? `\n\nFor truncated tasks, use bash_status({ taskId: "..." }) to retrieve full output.`
    : "";
  return `<system-reminder>\n[BACKGROUND BASH COMPLETED]\n${bullets}${tail}\n</system-reminder>`;
}

export function extractSessionID(value: unknown): string | undefined {
  if (!value || typeof value !== "object") return undefined;
  const record = value as Record<string, unknown>;
  for (const key of ["sessionID", "sessionId", "id"]) {
    if (typeof record[key] === "string") return record[key];
  }
  const info = record.info;
  if (info && typeof info === "object") {
    const nested = info as Record<string, unknown>;
    for (const key of ["sessionID", "sessionId", "id"]) {
      if (typeof nested[key] === "string") return nested[key];
    }
  }
  return undefined;
}

export function __resetBgNotificationStateForTests(): void {
  for (const state of sessionBgStates.values()) {
    if (state.debounceTimer) clearTimeout(state.debounceTimer);
  }
  sessionBgStates.clear();
}

async function drainCompletions({ ctx, directory, sessionID }: DrainContext): Promise<void> {
  try {
    const bridge = ctx.pool.getAnyActiveBridge(directory) ?? ctx.pool.getBridge(directory);
    const response = await bridge.send("bash_drain_completions", { session_id: sessionID });
    if (response.success === false) {
      sessionWarn(
        sessionID,
        `${LOG_PREFIX} drain failed: ${String(response.message ?? "unknown error")}`,
      );
      return;
    }
    ingestBgCompletions(sessionID, response.bg_completions);
  } catch (err) {
    sessionWarn(
      sessionID,
      `${LOG_PREFIX} drain failed: ${err instanceof Error ? err.message : String(err)}`,
    );
  }
}

function scheduleWake(
  state: SessionBgState,
  sendWake: (reminder: string) => Promise<void>,
  onSendFailure: (err: unknown) => void,
): void {
  // Race model: JS state changes are synchronous; awaits only happen before scheduling
  // drains and during final prompt delivery. Multiple hook invocations can interleave
  // only at those awaits, so we gate timer extension on the pending completion count.
  const now = Date.now();
  if (state.debounceTimer && state.pendingCompletions.length <= state.scheduledCompletionCount) {
    return;
  }
  if (state.firstCompletionAt === null) {
    state.firstCompletionAt = now;
    state.scheduledFireAt = now + DEBOUNCE_STEP_MS;
  } else {
    const previousFireAt = state.scheduledFireAt ?? now;
    state.scheduledFireAt = Math.min(
      previousFireAt + DEBOUNCE_STEP_MS,
      state.firstCompletionAt + DEBOUNCE_CAP_MS,
    );
  }
  state.scheduledCompletionCount = state.pendingCompletions.length;

  if (state.debounceTimer) clearTimeout(state.debounceTimer);
  const delay = state.retryDelayMs ?? Math.max(0, (state.scheduledFireAt ?? now) - now);
  state.debounceTimer = setTimeout(() => {
    const pending = state.pendingCompletions;
    const reminder = formatSystemReminder(pending);
    state.pendingCompletions = [];
    state.debounceTimer = null;
    state.firstCompletionAt = null;
    state.scheduledFireAt = null;
    state.scheduledCompletionCount = 0;
    void sendWake(reminder)
      .then(() => {
        state.retryDelayMs = null;
        state.wakeFiredThisIdle = true;
      })
      .catch((err) => {
        state.pendingCompletions = [...pending, ...state.pendingCompletions];
        state.retryDelayMs = Math.min((delay || DEBOUNCE_STEP_MS) * 2, DEBOUNCE_CAP_MS);
        onSendFailure(err);
        scheduleWake(state, sendWake, onSendFailure);
      });
  }, delay);
  state.debounceTimer.unref?.();
}

function stateFor(sessionID: string | undefined): SessionBgState {
  const now = Date.now();
  cleanupIdleSessionStates(now);
  const key = sessionID || DEFAULT_SESSION_ID;
  let state = sessionBgStates.get(key);
  if (!state) {
    state = {
      outstandingTaskIds: new Set(),
      pendingCompletions: [],
      debounceTimer: null,
      wakeFiredThisIdle: false,
      firstCompletionAt: null,
      scheduledFireAt: null,
      scheduledCompletionCount: 0,
      retryDelayMs: null,
      unknownCompletions: [],
      lastSeenAt: now,
    };
    sessionBgStates.set(key, state);
  } else {
    state.lastSeenAt = now;
  }
  return state;
}

function cleanupIdleSessionStates(now: number): void {
  const cutoff = now - SESSION_BG_STATE_IDLE_TTL_MS;
  for (const [sessionID, state] of sessionBgStates) {
    if (state.outstandingTaskIds.size > 0) continue;
    if (state.lastSeenAt >= cutoff) continue;
    if (state.debounceTimer) clearTimeout(state.debounceTimer);
    sessionBgStates.delete(sessionID);
  }
}

function bufferUnknownCompletion(state: SessionBgState, completion: BgCompletion): void {
  const now = Date.now();
  pruneUnknownCompletions(state, now);
  state.unknownCompletions = state.unknownCompletions.filter(
    (entry) => entry.completion.task_id !== completion.task_id,
  );
  state.unknownCompletions.push({ completion, receivedAt: now });
  if (state.unknownCompletions.length > UNKNOWN_COMPLETION_CAP) {
    state.unknownCompletions.splice(0, state.unknownCompletions.length - UNKNOWN_COMPLETION_CAP);
  }
}

function pruneUnknownCompletions(state: SessionBgState, now: number): void {
  state.unknownCompletions = state.unknownCompletions.filter(
    (entry) => now - entry.receivedAt <= UNKNOWN_COMPLETION_TTL_MS,
  );
}

function isBgCompletion(value: unknown): value is BgCompletion {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const completion = value as Record<string, unknown>;
  return (
    typeof completion.task_id === "string" &&
    typeof completion.status === "string" &&
    (typeof completion.exit_code === "number" || completion.exit_code === null) &&
    typeof completion.command === "string"
  );
}

function appendReminder(output: string, reminder: string): string {
  return output.length > 0 ? `${output}\n\n${reminder}` : reminder;
}

function formatCompletion(completion: BgCompletion): string {
  const status = formatStatus(completion);
  const duration = formatDuration(completion);
  const header = `- task ${completion.task_id} (${status}${duration ? `, ${duration}` : ""})`;
  const previewBlock = formatOutputPreview(completion);
  return previewBlock ? `${header}\n${previewBlock}` : header;
}

function formatOutputPreview(completion: BgCompletion): string {
  // Strip ANSI escape sequences defensively — most output passes through bash
  // compressors first, but raw stdout from non-compressed commands may still
  // contain colors that bloat the reminder. \x1b is the escape char.
  // biome-ignore lint/suspicious/noControlCharactersInRegex: ANSI escape stripping requires \x1b
  const ansiRegex = /\x1b\[[0-9;]*[a-zA-Z]/g;
  const raw = (completion.output_preview ?? "").replace(ansiRegex, "");
  if (!raw.trim()) return "";
  // Trim trailing newlines so the indented block doesn't end with a blank line
  // but preserve internal newlines so multi-line output stays readable.
  const trimmed = raw.replace(/\n+$/, "");
  const ellipsis = completion.output_truncated ? "…" : "";
  // 4-space indent makes the preview unambiguously a continuation of the
  // bullet above when the agent skims the reminder.
  const indented = trimmed
    .split("\n")
    .map((line) => `    ${line}`)
    .join("\n");
  return ellipsis ? `    ${ellipsis}\n${indented}` : indented;
}

function formatStatus(completion: BgCompletion): string {
  if (completion.status === "timed_out" || completion.status === "timeout") return "timed out";
  if (completion.status === "killed") return "killed";
  if (completion.exit_code !== null) return `exit ${completion.exit_code}`;
  return completion.status;
}

function formatDuration(completion: BgCompletion): string | null {
  const raw = completion.duration_ms ?? completion.runtime_ms ?? completion.runtime;
  if (typeof raw !== "number" || !Number.isFinite(raw) || raw < 0) return null;
  if (raw < 1000) return `${Math.round(raw)}ms`;
  const totalSeconds = Math.round(raw / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`;
}
