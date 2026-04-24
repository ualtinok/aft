import { readFileSync } from "node:fs";
import { warn } from "../logger";
import { rpcPortFilePath } from "./rpc-utils";

const MAX_RETRIES = 10;
const RETRY_DELAY_MS = 500;
const REQUEST_TIMEOUT_MS = 5000;

export class AftRpcClient {
  private port: number | null = null;
  private token: string | null = null;
  private portFilePath: string;
  private healthChecked = false;

  constructor(storageDir: string, directory: string) {
    this.portFilePath = rpcPortFilePath(storageDir, directory);
  }

  /** Call an RPC method. Retries port resolution if the server isn't ready yet. */
  async call<T = Record<string, unknown>>(
    method: string,
    params: Record<string, unknown> = {},
  ): Promise<T> {
    const info = await this.resolvePortInfo();
    if (!info) {
      throw new Error("AFT RPC server not available");
    }

    const response = await this.fetchWithTimeout(`http://127.0.0.1:${info.port}/rpc/${method}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ ...params, token: info.token }),
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`RPC ${method} failed (${response.status}): ${text}`);
    }

    return (await response.json()) as T;
  }

  /** Check if the RPC server is reachable. */
  async isAvailable(): Promise<boolean> {
    try {
      const port = await this.resolvePort();
      return port !== null;
    } catch {
      return false;
    }
  }

  private async resolvePort(): Promise<number | null> {
    return (await this.resolvePortInfo())?.port ?? null;
  }

  private async resolvePortInfo(): Promise<{ port: number; token: string | null } | null> {
    if (this.port && this.healthChecked) {
      return { port: this.port, token: this.token };
    }

    if (this.port) {
      const alive = await this.healthCheck(this.port);
      if (alive) {
        this.healthChecked = true;
        return { port: this.port, token: this.token };
      }
      this.port = null;
      this.token = null;
      this.healthChecked = false;
    }

    for (let attempt = 0; attempt < MAX_RETRIES; attempt++) {
      const info = this.readPortFile();
      if (info) {
        const alive = await this.healthCheck(info.port);
        if (alive) {
          this.port = info.port;
          this.token = info.token;
          this.healthChecked = true;
          return info;
        }
      }

      if (attempt < MAX_RETRIES - 1) {
        await new Promise((r) => setTimeout(r, RETRY_DELAY_MS));
      }
    }

    return null;
  }

  private readPortFile(): { port: number; token: string | null } | null {
    try {
      const content = readFileSync(this.portFilePath, "utf-8").trim();
      let port: number;
      let token: string | null;
      if (content.startsWith("{")) {
        const parsed = JSON.parse(content) as { port?: unknown; token?: unknown };
        port = typeof parsed.port === "number" ? parsed.port : Number.NaN;
        token = typeof parsed.token === "string" ? parsed.token : null;
      } else {
        warn("RPC port file uses legacy integer format; unauthenticated RPC is deprecated");
        port = Number.parseInt(content, 10);
        token = null;
      }
      if (Number.isNaN(port) || port <= 0 || port > 65535) {
        return null;
      }
      return { port, token };
    } catch {
      return null;
    }
  }

  private async healthCheck(port: number): Promise<boolean> {
    try {
      const response = await this.fetchWithTimeout(`http://127.0.0.1:${port}/health`, {
        method: "GET",
      });
      return response.ok;
    } catch {
      return false;
    }
  }

  private async fetchWithTimeout(url: string, options: RequestInit): Promise<Response> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
    try {
      return await fetch(url, { ...options, signal: controller.signal });
    } finally {
      clearTimeout(timeout);
    }
  }

  reset(): void {
    this.port = null;
    this.token = null;
    this.healthChecked = false;
  }
}
