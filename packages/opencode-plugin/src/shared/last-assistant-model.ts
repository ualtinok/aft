/**
 * Cache-busting fix: every PromptInput we send to OpenCode starts a new
 * assistant turn (via `noReply: false`) or stores a new user message
 * (via `noReply: true`). OpenCode's `createUserMessage` (in
 * `packages/opencode/src/session/prompt.ts`) only preserves the model
 * variant if we pass it explicitly — otherwise it falls back to
 * `agent.variant` or undefined, silently switching the next assistant
 * turn off the variant the previous assistant used. That switch evicts
 * the provider's prefix cache on every notification.
 *
 * We pin to the **last assistant message's** model + variant rather
 * than the last user message's, because:
 *
 * 1. The provider cache key is tied to what the assistant was actually
 *    using, not what the user typed last. A user can switch model
 *    mid-conversation; the prior assistant turn was already keyed to
 *    the old model, and that's what subsequent provider requests
 *    should preserve until a *real* user message changes it.
 * 2. Assistant messages reliably store both providerID/modelID
 *    (top-level, required) and variant (top-level, optional). User
 *    messages have a different shape (`info.model.{providerID, modelID,
 *    variant}`) and may not always carry it.
 *
 * Results are memoised per session for a short window so a batch of
 * warnings or bg-completions only hits the messages API once.
 */

interface SessionMessageInfo {
  id?: string;
  role?: string;
  modelID?: string;
  providerID?: string;
  variant?: string;
}

interface SessionMessage {
  info?: SessionMessageInfo;
}

interface OpenCodeClientShape {
  session?: {
    messages?: (input: { path: { id: string } }) => Promise<{ data?: SessionMessage[] }>;
  };
}

export interface LastAssistantModel {
  providerID: string;
  modelID: string;
  variant?: string;
}

interface CacheEntry {
  expiresAt: number;
  model: LastAssistantModel | null;
}

const CACHE_TTL_MS = 5_000;
const CACHE_MAX_ENTRIES = 100;
const cache = new Map<string, CacheEntry>();

/**
 * Returns the most recent assistant message's model + variant for the
 * session, or `null` when the session has no assistant messages or the
 * client doesn't expose `session.messages`. Result is cached for
 * {@link CACHE_TTL_MS} ms per session — long enough to coalesce a
 * batch of notifications, short enough that an actual mid-session
 * model change is picked up promptly.
 */
export async function getLastAssistantModel(
  client: unknown,
  sessionId: string,
): Promise<LastAssistantModel | null> {
  const now = Date.now();
  const cached = cache.get(sessionId);
  if (cached && cached.expiresAt > now) {
    cache.delete(sessionId);
    cache.set(sessionId, cached);
    return cached.model;
  }
  if (cached) cache.delete(sessionId);

  const c = client as OpenCodeClientShape;
  const fetcher = c?.session?.messages;
  if (typeof fetcher !== "function") {
    return null;
  }

  let messages: SessionMessage[];
  try {
    const result = await fetcher({ path: { id: sessionId } });
    messages = result?.data ?? [];
  } catch {
    // Don't cache failures — a transient API error shouldn't pin the
    // session's variant resolution to "unknown" for 5 s.
    return null;
  }

  for (let i = messages.length - 1; i >= 0; i--) {
    const info = messages[i]?.info;
    if (info?.role !== "assistant") continue;
    if (!info.providerID || !info.modelID) continue;
    const resolved: LastAssistantModel = {
      providerID: info.providerID,
      modelID: info.modelID,
      ...(info.variant ? { variant: info.variant } : {}),
    };
    setCache(sessionId, resolved);
    return resolved;
  }

  return null;
}

function setCache(sessionId: string, model: LastAssistantModel | null): void {
  if (cache.has(sessionId)) cache.delete(sessionId);
  cache.set(sessionId, { expiresAt: Date.now() + CACHE_TTL_MS, model });
  while (cache.size > CACHE_MAX_ENTRIES) {
    const oldest = cache.keys().next().value;
    if (typeof oldest !== "string") break;
    cache.delete(oldest);
  }
}

/** Test-only: drop the cache so unit tests can simulate fresh sessions. */
export function __resetLastAssistantModelCacheForTests(): void {
  cache.clear();
}
