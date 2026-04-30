/// <reference path="../../bun-test.d.ts" />

import { afterEach, beforeAll, describe, expect, test } from "bun:test";
import type { ChildProcess } from "node:child_process";
import { writeFile } from "node:fs/promises";
import {
  cleanupHarnesses,
  createHarness,
  type E2EHarness,
  type PreparedBinary,
  prepareBinary,
} from "./helpers.js";

const initialBinary = await prepareBinary();
const maybeDescribe = describe.skipIf(!initialBinary.binaryPath);

function childPid(bridge: E2EHarness["bridge"]): number {
  const child = (bridge as unknown as { process: ChildProcess | null }).process;
  const pid = child?.pid;
  if (pid === undefined) throw new Error("bridge child process is not spawned");
  return pid;
}

async function waitForExitHandler(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 500));
}

maybeDescribe("e2e bridge transport resilience (OpenCode)", () => {
  let preparedBinary: PreparedBinary = initialBinary;
  const harnesses: E2EHarness[] = [];

  beforeAll(async () => {
    preparedBinary = await prepareBinary();
  });

  afterEach(async () => {
    await cleanupHarnesses(harnesses);
  });

  async function harness(): Promise<E2EHarness> {
    const created = await createHarness(preparedBinary, {
      fixtureNames: [],
      timeoutMs: 10_000,
      bridgeOptions: { maxRestarts: 0 },
    });
    harnesses.push(created);
    await writeFile(created.path("sample.txt"), "alpha\nbeta\n", "utf8");
    return created;
  }

  test("a single timed-out request rejects without poisoning a following request", async () => {
    const h = await harness();
    await h.bridge.send("ping");
    const firstPid = childPid(h.bridge);

    const timedOut = h.bridge.send(
      "bash",
      { command: "sleep 1 && echo slow", timeout: 5_000, compressed: false },
      { transportTimeoutMs: 100 },
    );

    await expect(timedOut).rejects.toThrow('Request "bash"');
    expect(h.bridge.isAlive()).toBe(false);

    const after = await h.bridge.send("read", { file: h.path("sample.txt") });
    expect(after.success).toBe(true);
    expect(String(after.content ?? "")).toContain("alpha");
    expect(h.bridge.isAlive()).toBe(true);
    expect(childPid(h.bridge)).not.toBe(firstPid);
  });

  test("recovers with a fresh bridge after external SIGKILL", async () => {
    const h = await harness();

    const before = await h.bridge.send("read", { file: h.path("sample.txt") });
    expect(before.success).toBe(true);
    const killedPid = childPid(h.bridge);

    process.kill(killedPid, "SIGKILL");
    await waitForExitHandler();

    const after = await h.bridge.send("read", { file: h.path("sample.txt") });
    expect(after.success).toBe(true);
    expect(String(after.content ?? "")).toContain("beta");
    expect(childPid(h.bridge)).not.toBe(killedPid);
  });

  test("reserved command/method/session/lsp params route to the intended command", async () => {
    const h = await harness();

    const commandCollision = await h.bridge.send("bash", {
      command: "printf collision-ok",
      method: "not-a-bridge-method",
      session_id: "reserved-session",
      lsp_hints: { completions: ["test"] },
      timeout: 1_000,
      compressed: false,
    });
    expect(commandCollision.success).toBe(true);
    expect(commandCollision.output).toBe("collision-ok");

    const sessionSnapshot = await h.bridge.send("snapshot", {
      file: h.path("sample.txt"),
      session_id: "reserved-session",
    });
    expect(sessionSnapshot.success).toBe(true);

    const defaultHistory = await h.bridge.send("edit_history", { file: h.path("sample.txt") });
    expect(defaultHistory.success).toBe(true);
    expect(defaultHistory.entries).toEqual([]);

    const sessionHistory = await h.bridge.send("edit_history", {
      file: h.path("sample.txt"),
      session_id: "reserved-session",
    });
    expect(sessionHistory.success).toBe(true);
    expect((sessionHistory.entries as unknown[]).length).toBe(1);
  });

  test("reserved id params are rejected before corrupting bridge state", async () => {
    const h = await harness();

    await expect(h.bridge.send("read", { id: "1", file: h.path("sample.txt") })).rejects.toThrow(
      "params cannot contain reserved key 'id'",
    );

    const after = await h.bridge.send("read", { file: h.path("sample.txt") });
    expect(after.success).toBe(true);
    expect(String(after.content ?? "")).toContain("alpha");
  });
});
