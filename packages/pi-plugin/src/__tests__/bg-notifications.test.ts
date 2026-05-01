/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  __resetBgNotificationStateForTests,
  appendToolResultBgCompletions,
  cleanupIdleSessionStates,
  formatSystemReminder,
  handlePushedBgCompletion,
  handleTurnEndBgCompletions,
  resetBgWake,
  SESSION_BG_STATE_IDLE_TTL_MS,
  sessionBgStates,
  trackBgTask,
} from "../bg-notifications.js";
import type { PluginContext } from "../types.js";

type BridgeResponse = Record<string, unknown>;

afterEach(() => {
  __resetBgNotificationStateForTests();
});

describe("Pi background notifications", () => {
  test("formats system reminder bullets with status and duration", () => {
    expect(
      formatSystemReminder([
        {
          task_id: "d2ed3a9e",
          status: "completed",
          exit_code: 0,
          command: "cargo test --release",
          duration_ms: 83_000,
        },
        {
          task_id: "4f5b71c2",
          status: "timeout",
          exit_code: null,
          command: "npm install",
          duration_ms: 30_000,
        },
      ]),
    ).toBe(
      "<system-reminder>\n[BACKGROUND BASH COMPLETED]\n- task d2ed3a9e (exit 0, 1m 23s)\n- task 4f5b71c2 (timed out, 30s)\n</system-reminder>",
    );
  });

  test("tool_result mutation appends a reminder text block", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion("task-1", "echo done")],
    }));

    const content = await appendToolResultBgCompletions(
      { ctx, directory: "/tmp/project", sessionID: "s1" },
      [{ type: "text", text: "tool output" }],
    );

    expect(content).toHaveLength(2);
    expect(content?.[1]).toEqual({
      type: "text",
      text: "<system-reminder>\n[BACKGROUND BASH COMPLETED]\n- task task-1 (exit 0)\n</system-reminder>",
    });
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(0);
  });

  test("no-overhead path skips bridge drain when no tasks are outstanding", async () => {
    const send = mock(async () => ({ success: true, bg_completions: [] }));
    const { ctx } = harness(send);

    const content = await appendToolResultBgCompletions(
      { ctx, directory: "/tmp/project", sessionID: "s1" },
      [{ type: "text", text: "tool output" }],
    );

    expect(send).toHaveBeenCalledTimes(0);
    expect(content).toBeUndefined();
  });

  test("turn-end wake sends one runtime user message with reminder", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion("task-1", "npm test")],
    }));
    const sendUserMessage = mock(() => {});

    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(260);

    expect(sendUserMessage).toHaveBeenCalledTimes(1);
    expect(sendUserMessage.mock.calls[0][0]).toContain("- task task-1 (exit 0)");
    expect(sendUserMessage.mock.calls[0][0]).not.toContain(": npm test");
    // Regression: Pi's sendUserMessage rejects with "Agent is already
    // processing" when the agent is mid-turn unless we pass `deliverAs`.
    // The wake path must always pass `followUp` so a turn that starts
    // between our isActive check and the debounced send still queues
    // cleanly instead of throwing.
    expect(sendUserMessage.mock.calls[0][1]).toEqual({ deliverAs: "followUp" });
  });

  test("push completion lands in pending and wakes when idle", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({ success: true, bg_completions: [] }));
    const sendUserMessage = mock(() => {});

    await handlePushedBgCompletion(
      {
        ctx,
        directory: "/tmp/project",
        sessionID: "s1",
        runtime: { sendUserMessage },
      },
      completion("task-1", "npm test"),
    );
    await sleep(260);

    expect(sendUserMessage).toHaveBeenCalledTimes(1);
    expect(sendUserMessage.mock.calls[0][0]).toContain("- task task-1 (exit 0)");
    expect(sendUserMessage.mock.calls[0][0]).not.toContain(": npm test");
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(0);
  });

  test("push completion lands in pending without wake when active", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({ success: true, bg_completions: [] }));
    const sendUserMessage = mock(() => {});

    await handlePushedBgCompletion(
      {
        ctx,
        directory: "/tmp/project",
        sessionID: "s1",
        runtime: { sendUserMessage },
        isActive: () => true,
      },
      completion("task-1", "npm test"),
    );
    await sleep(260);

    expect(sendUserMessage).toHaveBeenCalledTimes(0);
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(1);
  });

  test("coalesces three idle completions into one notification", async () => {
    const responses = [
      { success: true, bg_completions: [completion("task-1", "one")] },
      { success: true, bg_completions: [completion("task-2", "two")] },
      { success: true, bg_completions: [completion("task-3", "three")] },
    ];
    const { ctx } = harness(() => responses.shift() ?? { success: true, bg_completions: [] });
    const sendUserMessage = mock(() => {});

    for (const taskId of ["task-1", "task-2", "task-3"]) trackBgTask("s1", taskId);
    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(50);
    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(50);
    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(520);

    expect(sendUserMessage).toHaveBeenCalledTimes(1);
    expect(String(sendUserMessage.mock.calls[0][0]).match(/^- task/gm)).toHaveLength(3);
  });

  test("debounce cap forces wake at about 1000ms", async () => {
    let index = 0;
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion(`task-${++index}`, `cmd-${index}`)],
    }));
    const sendUserMessage = mock(() => {});
    const started = Date.now();

    for (let task = 1; task <= 6; task++) trackBgTask("s1", `task-${task}`);
    for (let tick = 0; tick < 6; tick++) {
      await handleTurnEndBgCompletions({
        ctx,
        directory: "/tmp/project",
        sessionID: "s1",
        runtime: { sendUserMessage },
      });
      await sleep(190);
    }
    await sleep(120);

    expect(sendUserMessage).toHaveBeenCalledTimes(1);
    expect(Date.now() - started).toBeGreaterThanOrEqual(950);
    expect(Date.now() - started).toBeLessThan(1400);
  });

  test("rapid turn_end events are deduped after wake until input reset", async () => {
    const sendUserMessage = mock(() => {});
    let responses: BridgeResponse[] = [
      { success: true, bg_completions: [completion("task-1", "one")] },
    ];
    const { ctx } = harness(() => responses.shift() ?? { success: true, bg_completions: [] });

    trackBgTask("s1", "task-1");
    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(260);
    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(260);
    expect(sendUserMessage).toHaveBeenCalledTimes(1);

    resetBgWake("s1");
    responses = [{ success: true, bg_completions: [completion("task-2", "two")] }];
    trackBgTask("s1", "task-2");
    await handleTurnEndBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      runtime: { sendUserMessage },
    });
    await sleep(260);
    expect(sendUserMessage).toHaveBeenCalledTimes(2);
  });

  test("multi-session state is isolated", async () => {
    const { ctx } = harness((_, params) => ({
      success: true,
      bg_completions: [
        completion(params.session_id === "s1" ? "task-1" : "task-2", String(params.session_id)),
      ],
    }));

    trackBgTask("s1", "task-1");
    trackBgTask("s2", "task-2");
    const s1 = await appendToolResultBgCompletions(
      { ctx, directory: "/tmp/project", sessionID: "s1" },
      [{ type: "text", text: "one" }],
    );

    expect(s1?.[1].type === "text" ? s1[1].text : "").toContain("task-1");
    expect(sessionBgStates.get("s2")?.outstandingTaskIds.has("task-2")).toBe(true);

    const s2 = await appendToolResultBgCompletions(
      { ctx, directory: "/tmp/project", sessionID: "s2" },
      [{ type: "text", text: "two" }],
    );
    expect(s2?.[1].type === "text" ? s2[1].text : "").toContain("task-2");
  });

  test("cleanupIdleSessionStates evicts stale task-free sessions", () => {
    trackBgTask("stale", "task-stale");
    trackBgTask("fresh", "task-fresh");
    ingestCompletionForCleanup("stale", "task-stale");
    ingestCompletionForCleanup("fresh", "task-fresh");

    const now = Date.now();
    const stale = sessionBgStates.get("stale");
    const fresh = sessionBgStates.get("fresh");
    expect(stale).toBeDefined();
    expect(fresh).toBeDefined();
    if (!stale || !fresh) throw new Error("expected test states to exist");
    stale.lastSeenAt = now - SESSION_BG_STATE_IDLE_TTL_MS - 1;
    fresh.lastSeenAt = now;

    cleanupIdleSessionStates(now);

    expect(sessionBgStates.has("stale")).toBe(false);
    expect(sessionBgStates.has("fresh")).toBe(true);
  });

  test("drain failure does not break tool_result mutation", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => {
      throw new Error("bridge down");
    });

    const content = await appendToolResultBgCompletions(
      { ctx, directory: "/tmp/project", sessionID: "s1" },
      [{ type: "text", text: "normal" }],
    );

    expect(content).toBeUndefined();
  });
});

function harness(
  sendImpl: (
    command: string,
    params: Record<string, unknown>,
  ) => Promise<BridgeResponse> | BridgeResponse,
) {
  const bridge = {
    send: async (command: string, params: Record<string, unknown>) => sendImpl(command, params),
  };
  const ctx = {
    pool: {
      getAnyActiveBridge: () => bridge,
      getBridge: () => bridge,
    },
    config: {},
    storageDir: "/tmp/aft-test",
  } as unknown as PluginContext;
  return { ctx };
}

function completion(task_id: string, command: string) {
  return { task_id, status: "completed", exit_code: 0, command };
}

function ingestCompletionForCleanup(sessionID: string, taskID: string): void {
  const state = sessionBgStates.get(sessionID);
  if (!state) throw new Error(`missing state for ${sessionID}`);
  state.outstandingTaskIds.delete(taskID);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
