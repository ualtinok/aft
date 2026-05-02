/// <reference path="../bun-test.d.ts" />
import { afterEach, describe, expect, test } from "bun:test";
import {
  __resetLastAssistantModelCacheForTests,
  getLastAssistantModel,
} from "../shared/last-assistant-model.js";

afterEach(() => {
  __resetLastAssistantModelCacheForTests();
});

function makeClient(messages: unknown[]): unknown {
  return {
    session: {
      messages: async () => ({ data: messages }),
    },
  };
}

describe("getLastAssistantModel", () => {
  test("returns the last assistant message's model + variant", async () => {
    const client = makeClient([
      { info: { role: "user" } },
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);

    const result = await getLastAssistantModel(client, "session-1");
    expect(result).toEqual({
      providerID: "anthropic",
      modelID: "claude-opus-4-7",
      variant: "thinking",
    });
  });

  test("scans from newest to oldest", async () => {
    const client = makeClient([
      { info: { role: "user" } },
      {
        info: {
          role: "assistant",
          providerID: "openai",
          modelID: "gpt-4o",
          variant: "high",
        },
      },
      { info: { role: "user" } },
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);

    const result = await getLastAssistantModel(client, "session-1");
    // Most recent assistant message wins
    expect(result?.modelID).toBe("claude-opus-4-7");
    expect(result?.variant).toBe("thinking");
  });

  test("omits variant key when the message had none", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
        },
      },
    ]);

    const result = await getLastAssistantModel(client, "session-1");
    expect(result).toEqual({ providerID: "anthropic", modelID: "claude-opus-4-7" });
    // Important: the absent variant must not become an explicit `undefined` key,
    // or callers spreading the result will overwrite their own variant defaults.
    expect("variant" in (result as object)).toBe(false);
  });

  test("returns null when no assistant messages", async () => {
    const client = makeClient([{ info: { role: "user" } }]);
    const result = await getLastAssistantModel(client, "session-1");
    expect(result).toBeNull();
  });

  test("returns null when client has no session.messages", async () => {
    const client = { session: {} };
    const result = await getLastAssistantModel(client, "session-1");
    expect(result).toBeNull();
  });

  test("returns null when client is not an object", async () => {
    expect(await getLastAssistantModel(undefined, "session-1")).toBeNull();
    expect(await getLastAssistantModel(null, "session-1")).toBeNull();
  });

  test("skips assistant messages with malformed model fields", async () => {
    const client = makeClient([
      // Missing modelID
      { info: { role: "assistant", providerID: "anthropic" } },
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);

    const result = await getLastAssistantModel(client, "session-1");
    // The malformed last assistant message is skipped; the well-formed earlier one wins.
    expect(result?.modelID).toBe("claude-opus-4-7");
  });

  test("ignores user messages even when they carry a model field", async () => {
    const client = makeClient([
      // Older assistant turn — this is what we should pin to.
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
      // Newer user message with a different model selection. We should NOT
      // pick this up — preserving the assistant's prior model is what avoids
      // cache eviction across our notification path.
      {
        info: {
          role: "user",
          model: { providerID: "openai", modelID: "gpt-4o", variant: "high" },
        },
      },
    ]);

    const result = await getLastAssistantModel(client, "session-1");
    expect(result?.providerID).toBe("anthropic");
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
                  role: "assistant",
                  providerID: "anthropic",
                  modelID: "claude-opus-4-7",
                },
              },
            ],
          };
        },
      },
    };

    await getLastAssistantModel(client, "session-1");
    await getLastAssistantModel(client, "session-1");
    await getLastAssistantModel(client, "session-1");
    expect(callCount).toBe(1);

    // Different session bypasses the cache.
    await getLastAssistantModel(client, "session-2");
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
                  role: "assistant",
                  providerID: "anthropic",
                  modelID: "claude-opus-4-7",
                },
              },
            ],
          };
        },
      },
    };

    for (let i = 0; i < 101; i++) {
      await getLastAssistantModel(client, `session-${i}`);
    }
    await getLastAssistantModel(client, "session-0");

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
                  role: "assistant",
                  providerID: "anthropic",
                  modelID: "claude-opus-4-7",
                },
              },
            ],
          };
        },
      },
    };

    expect(await getLastAssistantModel(client, "session-1")).toBeNull();
    shouldFail = false;
    expect(await getLastAssistantModel(client, "session-1")).not.toBeNull();
    // Both calls actually hit the API — the failure was not cached.
    expect(callCount).toBe(2);
  });
});
