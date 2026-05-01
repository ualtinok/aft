/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import { __resetLastUserModelCacheForTests, getLastUserModel } from "../shared/last-user-model.js";

afterEach(() => {
  __resetLastUserModelCacheForTests();
});

function makeClient(messages: unknown[]): unknown {
  return {
    session: {
      messages: async () => ({ data: messages }),
    },
  };
}

describe("getLastUserModel", () => {
  test("returns the last user message's model + variant", async () => {
    const client = makeClient([
      {
        info: {
          role: "user",
          model: { providerID: "anthropic", modelID: "claude-opus-4-7", variant: "thinking" },
        },
      },
      { info: { role: "assistant" } },
    ]);

    const result = await getLastUserModel(client, "session-1");
    expect(result).toEqual({
      providerID: "anthropic",
      modelID: "claude-opus-4-7",
      variant: "thinking",
    });
  });

  test("scans from newest to oldest", async () => {
    const client = makeClient([
      {
        info: {
          role: "user",
          model: { providerID: "openai", modelID: "gpt-4o", variant: "high" },
        },
      },
      { info: { role: "assistant" } },
      {
        info: {
          role: "user",
          model: { providerID: "anthropic", modelID: "claude-opus-4-7", variant: "thinking" },
        },
      },
      { info: { role: "assistant" } },
    ]);

    const result = await getLastUserModel(client, "session-1");
    // Most recent user message wins
    expect(result?.modelID).toBe("claude-opus-4-7");
    expect(result?.variant).toBe("thinking");
  });

  test("omits variant key when the message had none", async () => {
    const client = makeClient([
      {
        info: {
          role: "user",
          model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
        },
      },
    ]);

    const result = await getLastUserModel(client, "session-1");
    expect(result).toEqual({ providerID: "anthropic", modelID: "claude-opus-4-7" });
    // Important: the absent variant must not become an explicit `undefined` key,
    // or callers spreading the result will overwrite their own variant defaults.
    expect("variant" in (result as object)).toBe(false);
  });

  test("returns null when no user messages", async () => {
    const client = makeClient([{ info: { role: "assistant" } }]);
    const result = await getLastUserModel(client, "session-1");
    expect(result).toBeNull();
  });

  test("returns null when client has no session.messages", async () => {
    const client = { session: {} };
    const result = await getLastUserModel(client, "session-1");
    expect(result).toBeNull();
  });

  test("returns null when client is not an object", async () => {
    expect(await getLastUserModel(undefined, "session-1")).toBeNull();
    expect(await getLastUserModel(null, "session-1")).toBeNull();
  });

  test("skips user messages with malformed model field", async () => {
    const client = makeClient([
      { info: { role: "user", model: { providerID: "anthropic" } } }, // missing modelID
      {
        info: {
          role: "user",
          model: { providerID: "anthropic", modelID: "claude-opus-4-7", variant: "thinking" },
        },
      },
    ]);

    const result = await getLastUserModel(client, "session-1");
    // The malformed last user message is skipped; the well-formed earlier one wins.
    expect(result?.modelID).toBe("claude-opus-4-7");
  });

  test("skips synthetic ignored user messages", async () => {
    const client = makeClient([
      {
        info: {
          role: "user",
          model: { providerID: "anthropic", modelID: "claude-opus-4-7", variant: "thinking" },
        },
      },
      {
        info: {
          role: "user",
          parts: [{ ignored: true }],
          model: { providerID: "openai", modelID: "gpt-4o", variant: "synthetic" },
        },
      },
    ]);

    const result = await getLastUserModel(client, "session-1");
    expect(result?.modelID).toBe("claude-opus-4-7");
    expect(result?.variant).toBe("thinking");
  });

  test("caches results within the TTL window per session", async () => {
    let callCount = 0;
    const client = {
      session: {
        messages: async () => {
          callCount++;
          return {
            data: [
              {
                info: {
                  role: "user",
                  model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
                },
              },
            ],
          };
        },
      },
    };

    await getLastUserModel(client, "session-1");
    await getLastUserModel(client, "session-1");
    await getLastUserModel(client, "session-1");
    expect(callCount).toBe(1);

    // Different session bypasses the cache.
    await getLastUserModel(client, "session-2");
    expect(callCount).toBe(2);
  });

  test("evicts least-recently-used cache entries over 100 sessions", async () => {
    let callCount = 0;
    const client = {
      session: {
        messages: async () => {
          callCount++;
          return {
            data: [
              {
                info: {
                  role: "user",
                  model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
                },
              },
            ],
          };
        },
      },
    };

    for (let i = 0; i < 101; i++) {
      await getLastUserModel(client, `session-${i}`);
    }
    await getLastUserModel(client, "session-0");

    expect(callCount).toBe(102);
  });

  test("does not cache fetch failures", async () => {
    let callCount = 0;
    let shouldFail = true;
    const client = {
      session: {
        messages: async () => {
          callCount++;
          if (shouldFail) throw new Error("transient API error");
          return {
            data: [
              {
                info: {
                  role: "user",
                  model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
                },
              },
            ],
          };
        },
      },
    };

    expect(await getLastUserModel(client, "session-1")).toBeNull();
    shouldFail = false;
    expect(await getLastUserModel(client, "session-1")).not.toBeNull();
    // Both calls actually hit the API — the failure was not cached.
    expect(callCount).toBe(2);
  });
});
