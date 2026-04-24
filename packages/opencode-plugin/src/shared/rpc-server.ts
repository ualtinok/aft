import { randomBytes } from "node:crypto";
import { mkdirSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { dirname } from "node:path";
import { log, warn } from "../logger";
import { rpcPortFilePath } from "./rpc-utils";

type RpcHandler = (params: Record<string, unknown>) => Promise<Record<string, unknown>>;

export class AftRpcServer {
  private server: Server | null = null;
  private port = 0;
  private token: string | null = null;
  private handlers = new Map<string, RpcHandler>();
  private portFilePath: string;

  constructor(storageDir: string, directory: string) {
    this.portFilePath = rpcPortFilePath(storageDir, directory);
  }

  /** Register an RPC method handler. */
  handle(method: string, handler: RpcHandler): void {
    this.handlers.set(method, handler);
  }

  /** Start the server on a random port, write port to disk. */
  async start(): Promise<number> {
    return new Promise((resolve, reject) => {
      const server = createServer((req, res) => this.dispatch(req, res));

      server.on("error", (err) => {
        warn(`RPC server error: ${err.message}`);
        reject(err);
      });

      server.listen(0, "127.0.0.1", () => {
        const addr = server.address();
        if (!addr || typeof addr === "string") {
          reject(new Error("Failed to get server address"));
          return;
        }
        this.port = addr.port;
        this.token = randomBytes(32).toString("hex");
        this.server = server;

        // Write port file atomically
        try {
          const dir = dirname(this.portFilePath);
          mkdirSync(dir, { recursive: true });
          const tmpPath = `${this.portFilePath}.tmp`;
          writeFileSync(tmpPath, JSON.stringify({ port: this.port, token: this.token }), "utf-8");
          renameSync(tmpPath, this.portFilePath);
          log(`RPC server listening on 127.0.0.1:${this.port}`);
        } catch (err) {
          warn(`Failed to write RPC port file: ${err}`);
        }

        resolve(this.port);
      });

      // Don't keep the process alive just for the RPC server
      server.unref();
    });
  }

  /** Stop the server and clean up port file. */
  stop(): void {
    if (this.server) {
      this.server.close();
      this.server = null;
    }
    this.token = null;
    try {
      unlinkSync(this.portFilePath);
    } catch {
      // ignore
    }
  }

  private dispatch(req: IncomingMessage, res: ServerResponse): void {
    const url = req.url ?? "";

    if (req.method === "GET" && url === "/health") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ ok: true, pid: process.pid }));
      return;
    }

    if (req.method !== "POST" || !url.startsWith("/rpc/")) {
      res.writeHead(404);
      res.end("Not Found");
      return;
    }

    const method = url.slice(5);
    const handler = this.handlers.get(method);
    if (!handler) {
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: `Unknown method: ${method}` }));
      return;
    }

    let body = "";
    req.on("data", (chunk: Buffer) => {
      body += chunk.toString();
      if (body.length > 1_048_576) {
        res.writeHead(413);
        res.end("Request too large");
        req.destroy();
      }
    });

    req.on("end", () => {
      let params: Record<string, unknown> = {};
      try {
        if (body.length > 0) {
          params = JSON.parse(body);
        }
      } catch {
        res.writeHead(400, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: "Invalid JSON" }));
        return;
      }

      if (params.token !== this.token) {
        res.writeHead(403, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: "Forbidden" }));
        return;
      }

      const { token: _token, ...handlerParams } = params;

      log(`RPC call: ${method} params=${JSON.stringify(handlerParams).slice(0, 200)}`);
      handler(handlerParams)
        .then((result) => {
          log(`RPC result: ${method} => ${JSON.stringify(result).slice(0, 200)}`);
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify(result));
        })
        .catch((err) => {
          log(`RPC error: ${method} => ${err}`);
          res.writeHead(500, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ error: String(err) }));
        });
    });
  }
}
