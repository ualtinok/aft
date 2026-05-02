/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  __resetBgNotificationStateForTests,
  appendInTurnBgCompletions,
  formatSystemReminder,
  handleIdleBgCompletions,
  handlePushedBgCompletion,
  ingestBgCompletions,
  resetBgWake,
  SESSION_BG_STATE_IDLE_TTL_MS,
  sessionBgStates,
  trackBgTask,
} from "../bg-notifications.js";
import { __resetLastAssistantModelCacheForTests } from "../shared/last-assistant-model.js";
import type { PluginContext } from "../types.js";

type BridgeResponse = Record<string, unknown>;

afterEach(() => {
  __resetBgNotificationStateForTests();
  __resetLastAssistantModelCacheForTests();
});

describe("OpenCode background notifications", () => {
  test("formats system reminder bullets with status and duration (no output, no preview block)", () => {
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
          status: "timed_out",
          exit_code: null,
          command: "npm install",
          duration_ms: 30_000,
        },
      ]),
    ).toBe(
      "<system-reminder>\n[BACKGROUND BASH COMPLETED]\n- task d2ed3a9e (exit 0, 1m 23s)\n- task 4f5b71c2 (timed out, 30s)\n</system-reminder>",
    );
  });

  test("formats system reminder with indented output preview when present", () => {
    expect(
      formatSystemReminder([
        {
          task_id: "abc123",
          status: "completed",
          exit_code: 0,
          command: "git status",
          duration_ms: 50,
          output_preview: "On branch main\nnothing to commit, working tree clean\n",
          output_truncated: false,
        },
      ]),
    ).toBe(
      "<system-reminder>\n[BACKGROUND BASH COMPLETED]\n- task abc123 (exit 0, 50ms)\n    On branch main\n    nothing to commit, working tree clean\n</system-reminder>",
    );
  });

  test("formats system reminder with truncation marker and bash_status pointer when truncated", () => {
    const reminder = formatSystemReminder([
      {
        task_id: "xyz789",
        status: "completed",
        exit_code: 1,
        command: "pytest",
        duration_ms: 12_000,
        output_preview: "...rest of trace\nFAILED tests/test_foo.py::test_bar - AssertionError\n",
        output_truncated: true,
      },
    ]);
    expect(reminder).toContain("- task xyz789 (exit 1, 12s)");
    expect(reminder).toContain("    …");
    expect(reminder).toContain("    ...rest of trace");
    expect(reminder).toContain("    FAILED tests/test_foo.py::test_bar - AssertionError");
    expect(reminder).toContain('For truncated tasks, use bash_status({ taskId: "..." })');
  });

  test("strips ANSI escape sequences from output preview", () => {
    const reminder = formatSystemReminder([
      {
        task_id: "ansi1",
        status: "completed",
        exit_code: 0,
        command: "ls --color",
        output_preview: "\x1b[34mfile.txt\x1b[0m\n\x1b[1;32mREADME\x1b[0m\n",
        output_truncated: false,
      },
    ]);
    expect(reminder).toContain("    file.txt");
    expect(reminder).toContain("    README");
    expect(reminder).not.toContain("\x1b[");
  });

  test("blank or whitespace-only preview produces no preview block", () => {
    const reminder = formatSystemReminder([
      {
        task_id: "empty1",
        status: "completed",
        exit_code: 0,
        command: "true",
        output_preview: "   \n\n",
        output_truncated: false,
      },
    ]);
    expect(reminder).toBe(
      "<system-reminder>\n[BACKGROUND BASH COMPLETED]\n- task empty1 (exit 0)\n</system-reminder>",
    );
  });

  test("in-turn delivery drains and appends reminder to tool output", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion("task-1", "echo done")],
    }));
    const output = { output: "tool output" };

    await appendInTurnBgCompletions({ ctx, directory: "/tmp/project", sessionID: "s1" }, output);

    expect(output.output).toContain("tool output\n\n<system-reminder>");
    expect(output.output).toContain("- task task-1 (exit 0)");
    expect(output.output).not.toContain(": echo done"); // command no longer in bullet
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(0);
    expect(sessionBgStates.get("s1")?.outstandingTaskIds.size).toBe(0);
  });

  test("no-overhead path skips bridge drain when no tasks are outstanding", async () => {
    const send = mock(async () => ({ success: true, bg_completions: [] }));
    const { ctx } = harness(send);
    const output = { output: "tool output" };

    await appendInTurnBgCompletions({ ctx, directory: "/tmp/project", sessionID: "s1" }, output);

    expect(send).toHaveBeenCalledTimes(0);
    expect(output.output).toBe("tool output");
  });

  test("turn-end wake sends one promptAsync message with reminder", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion("task-1", "npm test")],
    }));
    const promptAsync = mock(async () => {});

    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    const payload = promptAsync.mock.calls[0][0] as {
      body: { noReply: boolean; parts: Array<{ text: string }> };
    };
    expect(payload.body.noReply).toBe(false);
    expect(payload.body.parts[0].text).toContain("- task task-1 (exit 0)");
    expect(payload.body.parts[0].text).not.toContain(": npm test");
  });

  test("turn-end wake forwards last assistant model + variant to preserve prefix cache", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion("task-1", "npm test")],
    }));
    const promptAsync = mock(async () => {});
    const messages = mock(async () => ({
      data: [
        { info: { role: "user" } },
        {
          info: {
            role: "assistant",
            providerID: "anthropic",
            modelID: "claude-opus-4-7",
            variant: "thinking",
          },
        },
      ],
    }));

    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync, messages } },
    });
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    const payload = promptAsync.mock.calls[0][0] as {
      body: {
        noReply: boolean;
        parts: Array<{ text: string }>;
        model?: { providerID: string; modelID: string };
        variant?: string;
      };
    };
    expect(payload.body.model).toEqual({
      providerID: "anthropic",
      modelID: "claude-opus-4-7",
    });
    expect(payload.body.variant).toBe("thinking");
  });

  test("turn-end wake omits model/variant when no assistant history is reachable", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion("task-1", "npm test")],
    }));
    const promptAsync = mock(async () => {});

    // No `messages` on the client — getLastAssistantModel falls through to null,
    // and the wake should still go out without forging a fake model.
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    const payload = promptAsync.mock.calls[0][0] as {
      body: {
        noReply: boolean;
        parts: Array<{ text: string }>;
        model?: unknown;
        variant?: unknown;
      };
    };
    expect(payload.body.model).toBeUndefined();
    expect(payload.body.variant).toBeUndefined();
  });

  test("push completion lands in pending and wakes when idle", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({ success: true, bg_completions: [] }));
    const promptAsync = mock(async () => {});

    await handlePushedBgCompletion(
      {
        ctx,
        directory: "/tmp/project",
        sessionID: "s1",
        client: { session: { promptAsync } },
      },
      completion("task-1", "npm test"),
    );
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    const text = (promptAsync.mock.calls[0][0] as { body: { parts: Array<{ text: string }> } }).body
      .parts[0].text;
    expect(text).toContain("- task task-1 (exit 0)");
    expect(text).not.toContain(": npm test");
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(0);
  });

  test("buffers push completion received before task tracking", async () => {
    const { ctx } = harness(() => ({ success: true, bg_completions: [] }));
    const promptAsync = mock(async () => {});

    await handlePushedBgCompletion(
      { ctx, directory: "/tmp/project", sessionID: "s1", client: { session: { promptAsync } } },
      completion("task-1", "npm test"),
    );
    trackBgTask("s1", "task-1");
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    const text = (promptAsync.mock.calls[0][0] as { body: { parts: Array<{ text: string }> } }).body
      .parts[0].text;
    expect(text).toContain("- task task-1 (exit 0)");
  });

  test("failed wake keeps pending completions and retries", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({ success: true, bg_completions: [] }));
    const promptAsync = mock(async () => {
      throw new Error("send failed");
    });

    await handlePushedBgCompletion(
      { ctx, directory: "/tmp/project", sessionID: "s1", client: { session: { promptAsync } } },
      completion("task-1", "npm test"),
    );
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(1);
    expect(sessionBgStates.get("s1")?.debounceTimer).not.toBeNull();
  });

  test("push completion lands in pending without wake when active", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => ({ success: true, bg_completions: [] }));
    const promptAsync = mock(async () => {});

    await handlePushedBgCompletion(
      {
        ctx,
        directory: "/tmp/project",
        sessionID: "s1",
        client: { session: { promptAsync } },
        isActive: () => true,
      },
      completion("task-1", "npm test"),
    );
    await sleep(260);

    expect(promptAsync).toHaveBeenCalledTimes(0);
    expect(sessionBgStates.get("s1")?.pendingCompletions).toHaveLength(1);
  });

  test("coalesces three idle completions into one notification", async () => {
    const responses = [
      { success: true, bg_completions: [completion("task-1", "one")] },
      { success: true, bg_completions: [completion("task-2", "two")] },
      { success: true, bg_completions: [completion("task-3", "three")] },
    ];
    const { ctx } = harness(() => responses.shift() ?? { success: true, bg_completions: [] });
    const promptAsync = mock(async () => {});

    for (const taskId of ["task-1", "task-2", "task-3"]) trackBgTask("s1", taskId);
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(50);
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(50);
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(520);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    const text = (promptAsync.mock.calls[0][0] as { body: { parts: Array<{ text: string }> } }).body
      .parts[0].text;
    expect(text.match(/^- task/gm)).toHaveLength(3);
  });

  test("debounce cap forces wake at about 1000ms", async () => {
    let index = 0;
    const { ctx } = harness(() => ({
      success: true,
      bg_completions: [completion(`task-${++index}`, `cmd-${index}`)],
    }));
    const promptAsync = mock(async () => {});
    const started = Date.now();

    for (let task = 1; task <= 6; task++) trackBgTask("s1", `task-${task}`);
    for (let tick = 0; tick < 6; tick++) {
      await handleIdleBgCompletions({
        ctx,
        directory: "/tmp/project",
        sessionID: "s1",
        client: { session: { promptAsync } },
      });
      await sleep(190);
    }
    await sleep(120);

    expect(promptAsync).toHaveBeenCalledTimes(1);
    expect(Date.now() - started).toBeGreaterThanOrEqual(950);
    expect(Date.now() - started).toBeLessThan(1400);
  });

  test("rapid idle events are deduped after wake until chat message reset", async () => {
    const promptAsync = mock(async () => {});
    let responses: BridgeResponse[] = [
      { success: true, bg_completions: [completion("task-1", "one")] },
    ];
    const { ctx } = harness(() => responses.shift() ?? { success: true, bg_completions: [] });

    trackBgTask("s1", "task-1");
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(260);
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(260);
    expect(promptAsync).toHaveBeenCalledTimes(1);

    resetBgWake("s1");
    responses = [{ success: true, bg_completions: [completion("task-2", "two")] }];
    trackBgTask("s1", "task-2");
    await handleIdleBgCompletions({
      ctx,
      directory: "/tmp/project",
      sessionID: "s1",
      client: { session: { promptAsync } },
    });
    await sleep(260);
    expect(promptAsync).toHaveBeenCalledTimes(2);
  });

  test("multi-session state is isolated", async () => {
    const { ctx } = harness((_, params) => ({
      success: true,
      bg_completions: [
        completion(params.session_id === "s1" ? "task-1" : "task-2", String(params.session_id)),
      ],
    }));
    const out1 = { output: "one" };
    const out2 = { output: "two" };

    trackBgTask("s1", "task-1");
    trackBgTask("s2", "task-2");
    await appendInTurnBgCompletions({ ctx, directory: "/tmp/project", sessionID: "s1" }, out1);

    expect(out1.output).toContain("task-1");
    expect(out1.output).not.toContain("task-2");
    expect(sessionBgStates.get("s2")?.outstandingTaskIds.has("task-2")).toBe(true);

    await appendInTurnBgCompletions({ ctx, directory: "/tmp/project", sessionID: "s2" }, out2);
    expect(out2.output).toContain("task-2");
  });

  test("drain failure does not break normal tool output", async () => {
    trackBgTask("s1", "task-1");
    const { ctx } = harness(() => {
      throw new Error("bridge down");
    });
    const output = { output: "normal" };

    await appendInTurnBgCompletions({ ctx, directory: "/tmp/project", sessionID: "s1" }, output);

    expect(output.output).toBe("normal");
  });

  test("evicts task-free sessions after idle TTL on next access", () => {
    const originalDateNow = Date.now;
    let now = 1_000;
    Date.now = () => now;

    try {
      trackBgTask("stale", "task-1");
      ingestBgCompletions("stale", [completion("task-1", "done")]);
      expect(sessionBgStates.get("stale")?.outstandingTaskIds.size).toBe(0);

      now += SESSION_BG_STATE_IDLE_TTL_MS + 1;
      trackBgTask("active", "task-2");

      expect(sessionBgStates.has("stale")).toBe(false);
      expect(sessionBgStates.has("active")).toBe(true);
    } finally {
      Date.now = originalDateNow;
    }
  });

  test("does not evict sessions with outstanding tasks regardless of age", () => {
    const originalDateNow = Date.now;
    let now = 1_000;
    Date.now = () => now;

    try {
      trackBgTask("old-active", "task-1");

      now += SESSION_BG_STATE_IDLE_TTL_MS + 1;
      trackBgTask("new-active", "task-2");

      expect(sessionBgStates.get("old-active")?.outstandingTaskIds.has("task-1")).toBe(true);
      expect(sessionBgStates.has("new-active")).toBe(true);
    } finally {
      Date.now = originalDateNow;
    }
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
    client: {},
    config: {},
    storageDir: "/tmp/aft-test",
  } as unknown as PluginContext;
  return { ctx };
}

function completion(task_id: string, command: string) {
  return { task_id, status: "completed", exit_code: 0, command };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
