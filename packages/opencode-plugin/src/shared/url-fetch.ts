import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { log, warn } from "../logger";

/** Max response body size (10 MB) */
const MAX_RESPONSE_BYTES = 10 * 1024 * 1024;
/** Cache TTL: 1 day */
const CACHE_TTL_MS = 24 * 60 * 60 * 1000;
/** Fetch timeout: 30 seconds */
const FETCH_TIMEOUT_MS = 30_000;

interface CacheMeta {
  url: string;
  contentType: string;
  extension: string;
  fetchedAt: number;
}

function cacheDir(storageDir: string): string {
  return join(storageDir, "url_cache");
}

function hashUrl(url: string): string {
  return createHash("sha256").update(url).digest("hex").slice(0, 16);
}

function metaPath(storageDir: string, hash: string): string {
  return join(cacheDir(storageDir), `${hash}.meta.json`);
}

function contentPath(storageDir: string, hash: string, extension: string): string {
  return join(cacheDir(storageDir), `${hash}${extension}`);
}

/**
 * Map a Content-Type header to a file extension AFT can parse.
 * Returns null for unsupported types.
 */
function resolveExtension(contentType: string): string | null {
  // Normalize: strip parameters (after `;`) AND pick the first value if the server
  // echoed back a comma-separated list (GitHub API does this for /readme endpoints).
  const lower = contentType.toLowerCase().split(";")[0].split(",")[0].trim();
  if (
    lower === "text/html" ||
    lower === "application/xhtml+xml" ||
    lower === "application/vnd.github.html" ||
    lower === "application/vnd.github+html"
  ) {
    return ".html";
  }
  if (
    lower === "text/markdown" ||
    lower === "text/x-markdown" ||
    lower === "application/markdown" ||
    // GitHub API raw content type — returns raw markdown for README endpoints
    lower === "application/vnd.github.raw" ||
    lower === "application/vnd.github+raw" ||
    lower === "application/vnd.github.v3.raw"
  ) {
    return ".md";
  }
  if (lower === "text/plain") {
    // treat plain text as markdown so aft_outline can show headings if present
    return ".md";
  }
  return null;
}

/**
 * Fetch a URL to a cached temp file. Uses disk cache with 1-day TTL.
 * Returns the cached file path the Rust outline/zoom command can read.
 * Throws on errors (invalid URL, network failure, unsupported content type, oversized body).
 */
export async function fetchUrlToTempFile(url: string, storageDir: string): Promise<string> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new Error(`Invalid URL: ${url}`);
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error(`Only http:// and https:// URLs are supported, got: ${parsed.protocol}`);
  }

  const dir = cacheDir(storageDir);
  mkdirSync(dir, { recursive: true });

  const hash = hashUrl(url);
  const metaFile = metaPath(storageDir, hash);

  // Check cache
  if (existsSync(metaFile)) {
    try {
      const meta = JSON.parse(readFileSync(metaFile, "utf8")) as CacheMeta;
      const age = Date.now() - meta.fetchedAt;
      const cached = contentPath(storageDir, hash, meta.extension);
      if (age < CACHE_TTL_MS && existsSync(cached)) {
        log(`URL cache hit: ${url} (${Math.round(age / 1000)}s old)`);
        return cached;
      }
    } catch {
      // corrupted meta, re-fetch
    }
  }

  log(`Fetching URL: ${url}`);

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  let response: Response;
  try {
    response = await fetch(url, {
      signal: controller.signal,
      redirect: "follow",
      headers: {
        "user-agent": "aft-opencode-plugin",
        // Prioritize markdown for content-negotiating servers (GitHub API, many docs sites).
        // `application/vnd.github.raw` is GitHub's custom type — returns raw markdown from
        // repo file and readme endpoints. Falls back to HTML if markdown is not available.
        accept:
          "application/vnd.github.raw, text/markdown, text/x-markdown, text/html;q=0.9, text/plain;q=0.5",
      },
    });
  } catch (err) {
    throw new Error(`Failed to fetch ${url}: ${(err as Error).message}`);
  } finally {
    clearTimeout(timer);
  }

  if (!response.ok) {
    throw new Error(`HTTP ${response.status} ${response.statusText} fetching ${url}`);
  }

  const contentType = response.headers.get("content-type") || "text/plain";
  const extension = resolveExtension(contentType);
  if (!extension) {
    throw new Error(
      `Unsupported content type '${contentType}' for ${url}. Supported: text/html, text/markdown, text/plain`,
    );
  }

  const lengthHeader = response.headers.get("content-length");
  if (lengthHeader) {
    const length = Number.parseInt(lengthHeader, 10);
    if (Number.isFinite(length) && length > MAX_RESPONSE_BYTES) {
      throw new Error(`Response too large: ${length} bytes (max ${MAX_RESPONSE_BYTES})`);
    }
  }

  // Stream with size cap
  const reader = response.body?.getReader();
  if (!reader) {
    throw new Error(`Failed to read response body for ${url}`);
  }
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    if (value) {
      total += value.length;
      if (total > MAX_RESPONSE_BYTES) {
        reader.cancel().catch(() => {});
        throw new Error(`Response exceeded ${MAX_RESPONSE_BYTES} bytes, aborted`);
      }
      chunks.push(value);
    }
  }

  // Write content and meta atomically
  const body = Buffer.concat(chunks);
  const contentFile = contentPath(storageDir, hash, extension);
  const tmpContent = `${contentFile}.tmp-${process.pid}`;
  writeFileSync(tmpContent, body);
  const { renameSync } = await import("node:fs");
  renameSync(tmpContent, contentFile);

  const meta: CacheMeta = {
    url,
    contentType,
    extension,
    fetchedAt: Date.now(),
  };
  const tmpMeta = `${metaFile}.tmp-${process.pid}`;
  writeFileSync(tmpMeta, JSON.stringify(meta));
  renameSync(tmpMeta, metaFile);

  log(`URL cached (${total} bytes): ${url}`);
  return contentFile;
}

/**
 * Remove cache entries older than TTL. Called periodically at plugin startup.
 */
export function cleanupUrlCache(storageDir: string): void {
  const dir = cacheDir(storageDir);
  if (!existsSync(dir)) return;

  let removed = 0;
  try {
    for (const entry of readdirSync(dir)) {
      if (!entry.endsWith(".meta.json")) continue;
      const metaFile = join(dir, entry);
      try {
        const meta = JSON.parse(readFileSync(metaFile, "utf8")) as CacheMeta;
        const age = Date.now() - meta.fetchedAt;
        if (age > CACHE_TTL_MS) {
          const hash = entry.slice(0, -".meta.json".length);
          const content = contentPath(storageDir, hash, meta.extension);
          if (existsSync(content)) unlinkSync(content);
          unlinkSync(metaFile);
          removed++;
        }
      } catch {
        // corrupted meta, remove it too
        try {
          unlinkSync(metaFile);
          removed++;
        } catch {}
      }
    }
  } catch (err) {
    warn(`URL cache cleanup failed: ${(err as Error).message}`);
    return;
  }
  if (removed > 0) {
    log(`URL cache cleanup: removed ${removed} stale entries`);
  }
}
