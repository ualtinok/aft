import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const TAG = "[aft-plugin]";
const logFile = path.join(os.tmpdir(), "aft-plugin.log");

/**
 * When AFT_LOG_STDERR=1, logs go to stderr (useful for subprocess tests that
 * capture stderr output). Otherwise logs go to the temp file.
 */
const useStderr = process.env.AFT_LOG_STDERR === "1";

let buffer: string[] = [];
let flushTimer: ReturnType<typeof setTimeout> | null = null;
const FLUSH_INTERVAL_MS = 500;
const BUFFER_SIZE_LIMIT = 50;

function flush(): void {
  if (buffer.length === 0) return;
  const data = buffer.join("");
  buffer = [];
  try {
    if (useStderr) {
      process.stderr.write(data);
    } else {
      fs.appendFileSync(logFile, data);
    }
  } catch {
    // Intentional: logging must never throw
  }
}

function scheduleFlush(): void {
  if (flushTimer) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    flush();
  }, FLUSH_INTERVAL_MS);
  // Don't prevent Node from exiting
  if (flushTimer && typeof flushTimer === "object" && "unref" in flushTimer) {
    flushTimer.unref();
  }
}

function write(level: string, message: string, data?: unknown): void {
  try {
    const timestamp = new Date().toISOString();
    const serialized = data === undefined ? "" : ` ${JSON.stringify(data)}`;
    const line = `[${timestamp}] ${level} ${TAG} ${message}${serialized}\n`;
    if (useStderr) {
      // Write immediately in stderr mode (subprocess tests need it before exit)
      process.stderr.write(line);
      return;
    }
    buffer.push(line);
    if (buffer.length >= BUFFER_SIZE_LIMIT) {
      flush();
    } else {
      scheduleFlush();
    }
  } catch {
    // Intentional: logging must never throw
  }
}
export function log(message: string, data?: unknown): void {
  write("INFO", message, data);
}

export function warn(message: string, data?: unknown): void {
  write("WARN", message, data);
}

export function error(message: string, data?: unknown): void {
  write("ERROR", message, data);
}

export function getLogFilePath(): string {
  return logFile;
}
