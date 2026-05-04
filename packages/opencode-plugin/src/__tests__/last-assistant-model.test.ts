/// <reference path="../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { getLastAssistantModel, resolvePromptContext } from "../shared/last-assistant-model.js";

function makeClient(messages: unknown[]) {
  return {
    session: {
      messages: async (_input: { path: { id: string } }) => ({ data: messages }),
    },
  };
}

describe("resolvePromptContext (xtra-style: reads from messages API)", () => {
  test("reads flat-shape AssistantMessage info", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          agent: "build",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);
    const result = await resolvePromptContext(client, "s1");
    expect(result).toEqual({
      agent: "build",
      model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
      variant: "thinking",
    });
  });

  test("reads nested-shape UserMessage info as fallback when no assistant has fields", async () => {
    const client = makeClient([
      {
        info: {
          role: "user",
          agent: "build",
          model: {
            providerID: "anthropic",
            modelID: "claude-opus-4-7",
            variant: "thinking",
          },
        },
      },
    ]);
    const result = await resolvePromptContext(client, "s1");
    expect(result).toEqual({
      agent: "build",
      model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
      variant: "thinking",
    });
  });

  test("prefers assistant role over user role", async () => {
    const client = makeClient([
      {
        info: {
          role: "user",
          agent: "build",
          model: { providerID: "openai", modelID: "gpt-4o", variant: "high" },
        },
      },
      {
        info: {
          role: "assistant",
          agent: "build",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);
    const result = await resolvePromptContext(client, "s1");
    expect(result?.model?.modelID).toBe("claude-opus-4-7");
    expect(result?.variant).toBe("thinking");
  });

  test("walks newest-first within same role", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          agent: "build",
          providerID: "openai",
          modelID: "gpt-4o",
          variant: "high",
        },
      },
      {
        info: {
          role: "assistant",
          agent: "build",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);
    const result = await resolvePromptContext(client, "s1");
    expect(result?.model?.modelID).toBe("claude-opus-4-7");
  });

  test("merges fields across messages — agent from one, model+variant from another", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
          // no agent
        },
      },
      {
        // older user message provides agent
        info: {
          role: "user",
          agent: "build",
        },
      },
    ]);
    const result = await resolvePromptContext(client, "s1");
    expect(result).toEqual({
      agent: "build",
      model: { providerID: "anthropic", modelID: "claude-opus-4-7" },
      variant: "thinking",
    });
  });

  test("returns null on empty messages array", async () => {
    const client = makeClient([]);
    expect(await resolvePromptContext(client, "s1")).toBeNull();
  });

  test("returns null when client.session.messages is unavailable", async () => {
    const result = await resolvePromptContext({}, "s1");
    expect(result).toBeNull();
  });

  test("returns null when the messages API throws", async () => {
    const client = {
      session: {
        messages: async () => {
          throw new Error("boom");
        },
      },
    };
    expect(await resolvePromptContext(client, "s1")).toBeNull();
  });

  test("accepts response shape without `data` wrapper (raw array)", async () => {
    const client = {
      session: {
        messages: async () => [
          {
            info: {
              role: "assistant",
              agent: "build",
              providerID: "anthropic",
              modelID: "claude-opus-4-7",
            },
          },
        ],
      },
    };
    const result = await resolvePromptContext(client, "s1");
    expect(result?.agent).toBe("build");
  });

  test("ignores model entries missing providerID or modelID", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          // missing modelID
        },
      },
    ]);
    expect(await resolvePromptContext(client, "s1")).toBeNull();
  });
});

describe("getLastAssistantModel (compatibility shim)", () => {
  test("returns the resolved model + variant", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
          variant: "thinking",
        },
      },
    ]);
    expect(await getLastAssistantModel(client, "s1")).toEqual({
      providerID: "anthropic",
      modelID: "claude-opus-4-7",
      variant: "thinking",
    });
  });

  test("returns null when no model can be resolved", async () => {
    const client = makeClient([{ info: { role: "assistant", agent: "build" } }]);
    expect(await getLastAssistantModel(client, "s1")).toBeNull();
  });

  test("omits variant key when none was found", async () => {
    const client = makeClient([
      {
        info: {
          role: "assistant",
          providerID: "anthropic",
          modelID: "claude-opus-4-7",
        },
      },
    ]);
    const result = await getLastAssistantModel(client, "s1");
    expect(result).toEqual({
      providerID: "anthropic",
      modelID: "claude-opus-4-7",
    });
    expect("variant" in (result as object)).toBe(false);
  });
});
