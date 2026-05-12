/// <reference path="../bun-test.d.ts" />
/**
 * Regression coverage for the Effect runtime contract.
 *
 * Oracle audit (v0.19.5..HEAD): "No tests exercise the Effect-returning ask()
 * branch — every regression test stubbed `ask` as `mock(async () => {})`,
 * which is why a broken Effect.runPromise slipped through to production".
 *
 * These tests use the SAME `effect` package version that
 * `@opencode-ai/plugin` ships with at runtime, so they pin the actual
 * deny-evaluation contract: rules MUST execute, allows MUST resolve cleanly,
 * and denies MUST surface as rejected promises with the underlying error
 * message intact for `askEditPermission`'s try/catch to read.
 */
import { describe, expect, test } from "bun:test";
import type { ToolContext } from "@opencode-ai/plugin";
import { Effect } from "effect";
import {
  askEditPermission,
  askGlobPermission,
  askGrepPermission,
  runAsk,
} from "../tools/permissions.js";

describe("runAsk + Effect (real runtime)", () => {
  test("Effect.succeed resolves cleanly through runAsk (allow path)", async () => {
    let executed = false;
    const ask = Effect.sync(() => {
      executed = true;
    });
    await runAsk(ask);
    // The whole point of the v0.19.5 fix: the Effect must actually RUN.
    // Old buggy code did `await effect` and the body never executed.
    expect(executed).toBe(true);
  });

  test("Effect.fail rejects runAsk with the underlying Error (deny path)", async () => {
    const denied = Effect.fail(
      new Error("Permission denied: bash deny rule"),
    ) as unknown as Effect.Effect<void>;
    await expect(runAsk(denied)).rejects.toThrow("Permission denied: bash deny rule");
  });

  test("askEditPermission returns undefined when the Effect resolves", async () => {
    const ctx = makeMockContext(() => Effect.sync(() => {}));
    const result = await askEditPermission(ctx, ["src/foo.ts"]);
    // Convention: undefined = allowed; a string = denial reason.
    expect(result).toBeUndefined();
  });

  test("askEditPermission reports unsupported host when context.ask is missing", async () => {
    const ctx = {
      ...makeMockContext(() => Effect.sync(() => {})),
      ask: undefined,
    } as unknown as ToolContext;
    const result = await askEditPermission(ctx, ["src/foo.ts"]);
    expect(result).toContain("OpenCode 1.14.39 or newer");
    expect(result).not.toContain("denied");
  });

  test("askEditPermission surfaces deny message when the Effect fails", async () => {
    const ctx = makeMockContext(
      () =>
        Effect.fail(
          new Error("Permission denied for src/foo.ts"),
        ) as unknown as Effect.Effect<void>,
    );
    const result = await askEditPermission(ctx, ["src/foo.ts"]);
    expect(result).toBe("Permission denied for src/foo.ts");
  });

  test("askEditPermission falls back to default message when Effect fails without a useful message", async () => {
    // Effect.die / Effect.fail with empty message — defect propagation.
    const ctx = makeMockContext(() => Effect.fail(new Error("")) as unknown as Effect.Effect<void>);
    const result = await askEditPermission(ctx, ["src/foo.ts"]);
    expect(result).toBe("Permission denied.");
  });

  test("ask Effect actually executes — proves we did not regress to silent await", async () => {
    // This is the exact regression Oracle flagged. If runAsk reverts to
    // `await maybe` (without Effect.runPromise), this test fails because
    // the body of Effect.sync never runs.
    let askWasInvoked = false;
    const ctx = makeMockContext(() =>
      Effect.sync(() => {
        askWasInvoked = true;
      }),
    );
    await askEditPermission(ctx, ["src/foo.ts"]);
    expect(askWasInvoked).toBe(true);
  });
});

describe("askGrepPermission / askGlobPermission (real Effect runtime)", () => {
  test("askGrepPermission returns undefined on allow", async () => {
    const ctx = makeMockContext(() => Effect.sync(() => {}));
    const result = await askGrepPermission(ctx, "TODO");
    expect(result).toBeUndefined();
  });

  test("askGrepPermission surfaces deny message", async () => {
    const ctx = makeMockContext(
      () => Effect.fail(new Error("Grep denied by policy")) as unknown as Effect.Effect<void>,
    );
    const result = await askGrepPermission(ctx, "TODO");
    expect(result).toBe("Grep denied by policy");
  });

  test("askGrepPermission falls back to default message when Effect fails without one", async () => {
    const ctx = makeMockContext(() => Effect.fail(new Error("")) as unknown as Effect.Effect<void>);
    const result = await askGrepPermission(ctx, "TODO");
    expect(result).toBe("Permission denied (grep).");
  });

  test("askGrepPermission forwards pattern + path + include in the ask payload", async () => {
    let observed: { permission?: string; patterns?: string[]; metadata?: Record<string, unknown> } =
      {};
    const ctx = makeMockContext(
      (args) =>
        Effect.sync(() => {
          observed = args as typeof observed;
        }) as unknown as Effect.Effect<void>,
    );
    await askGrepPermission(ctx, "TODO\\b", { path: "src", include: "*.ts" });
    expect(observed.permission).toBe("grep");
    expect(observed.patterns).toEqual(["TODO\\b"]);
    expect(observed.metadata).toEqual({ pattern: "TODO\\b", path: "src", include: "*.ts" });
  });

  test("askGlobPermission returns undefined on allow", async () => {
    const ctx = makeMockContext(() => Effect.sync(() => {}));
    const result = await askGlobPermission(ctx, "**/*.ts");
    expect(result).toBeUndefined();
  });

  test("askGlobPermission surfaces deny message", async () => {
    const ctx = makeMockContext(
      () => Effect.fail(new Error("Glob denied by policy")) as unknown as Effect.Effect<void>,
    );
    const result = await askGlobPermission(ctx, "**/*.ts");
    expect(result).toBe("Glob denied by policy");
  });

  test("askGlobPermission falls back to default message when Effect fails without one", async () => {
    const ctx = makeMockContext(() => Effect.fail(new Error("")) as unknown as Effect.Effect<void>);
    const result = await askGlobPermission(ctx, "**/*.ts");
    expect(result).toBe("Permission denied (glob).");
  });

  test("askGlobPermission forwards pattern + path in the ask payload", async () => {
    let observed: { permission?: string; patterns?: string[]; metadata?: Record<string, unknown> } =
      {};
    const ctx = makeMockContext(
      (args) =>
        Effect.sync(() => {
          observed = args as typeof observed;
        }) as unknown as Effect.Effect<void>,
    );
    await askGlobPermission(ctx, "**/*.test.ts", { path: "src" });
    expect(observed.permission).toBe("glob");
    expect(observed.patterns).toEqual(["**/*.test.ts"]);
    expect(observed.metadata).toEqual({ pattern: "**/*.test.ts", path: "src" });
  });
});

function makeMockContext(askFn: ToolContext["ask"]): ToolContext {
  return {
    sessionID: "test-session",
    messageID: "test-message",
    agent: "test-agent",
    directory: "/tmp/aft-permissions-effect-test",
    worktree: "/tmp/aft-permissions-effect-test",
    abort: new AbortController().signal,
    metadata: () => {},
    ask: askFn,
  };
}
