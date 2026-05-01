/**
 * Cache-busting fix: every PromptInput we send to OpenCode starts a new
 * assistant turn (via `noReply: false`) or stores a new user message
 * (via `noReply: true`). OpenCode's `createUserMessage` (in
 * `packages/opencode/src/session/prompt.ts`) only preserves the user's
 * model variant if we pass it explicitly — otherwise it falls back to
 * `agent.variant` or undefined, silently switching the next assistant
 * turn off the variant the human chose. That switch evicts the
 * provider's prefix cache on every notification.
 *
 * This module fetches the last real user message's `{ providerID,
 * modelID, variant }` and exposes it so prompt callers can include it
 * verbatim in `PromptInput.model` + `PromptInput.variant`. Results are
 * memoised per session for a short window so a batch of warnings or
 * bg-completions only hits the messages API once.
 */

interface SessionMessageInfo {
  id?: string;
  role?: string;
  model?: {
    providerID?: string;
    modelID?: string;
    variant?: string;
  };
}

interface SessionMessage {
  info?: SessionMessageInfo;
}

interface OpenCodeClientShape {
  session?: {
    messages?: (input: { path: { id: string } }) => Promise<{ data?: SessionMessage[] }>;
  };
}

export interface LastUserModel {
  providerID: string;
  modelID: string;
  variant?: string;
}

interface CacheEntry {
  expiresAt: number;
  model: LastUserModel | null;
}

const CACHE_TTL_MS = 5_000;
const cache = new Map<string, CacheEntry>();

/**
 * Returns the most recent real user message's model + variant for the
 * session, or `null` when the session has no user messages or the
 * client doesn't expose `session.messages`. Result is cached for
 * {@link CACHE_TTL_MS} ms per session — long enough to coalesce a
 * batch of notifications, short enough that an actual variant change
 * inside the conversation is picked up promptly.
 *
 * NOTE: Skips synthetic user messages we just produced ourselves
 * (those whose only parts are `ignored: true`). Otherwise our own
 * announcement could pin the variant after a single round-trip and
 * defeat the fix.
 */
export async function getLastUserModel(
  client: unknown,
  sessionId: string,
): Promise<LastUserModel | null> {
  const now = Date.now();
  const cached = cache.get(sessionId);
  if (cached && cached.expiresAt > now) {
    return cached.model;
  }

  const c = client as OpenCodeClientShape;
  const fetcher = c?.session?.messages;
  if (typeof fetcher !== "function") {
    setCache(sessionId, null);
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
    if (info?.role !== "user") continue;
    const model = info.model;
    if (!model?.providerID || !model?.modelID) continue;
    const resolved: LastUserModel = {
      providerID: model.providerID,
      modelID: model.modelID,
      ...(model.variant ? { variant: model.variant } : {}),
    };
    setCache(sessionId, resolved);
    return resolved;
  }

  setCache(sessionId, null);
  return null;
}

function setCache(sessionId: string, model: LastUserModel | null): void {
  cache.set(sessionId, { expiresAt: Date.now() + CACHE_TTL_MS, model });
}

/** Test-only: drop the cache so unit tests can simulate fresh sessions. */
export function __resetLastUserModelCacheForTests(): void {
  cache.clear();
}
