/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { AftRpcClient } from "../shared/rpc-client.js";
import { AftRpcServer } from "../shared/rpc-server.js";
import { rpcPortFilePath } from "../shared/rpc-utils.js";

const tempRoots = new Set<string>();

function makeFixture() {
  const root = mkdtempSync(join(tmpdir(), "aft-rpc-auth-"));
  tempRoots.add(root);
  return { storageDir: join(root, "storage"), directory: join(root, "project") };
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("AFT RPC auth", () => {
  test("writes token to port file and requires it for requests", async () => {
    const fixture = makeFixture();
    const server = new AftRpcServer(fixture.storageDir, fixture.directory);
    server.handle("echo", async (params) => ({ ok: true, params }));

    try {
      const port = await server.start();
      const portFile = JSON.parse(
        readFileSync(rpcPortFilePath(fixture.storageDir, fixture.directory), "utf-8"),
      ) as { port: number; token: string };
      expect(portFile.port).toBe(port);
      expect(portFile.token).toMatch(/^[0-9a-f]{64}$/);

      const forbidden = await fetch(`http://127.0.0.1:${port}/rpc/echo`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ value: 1 }),
      });
      expect(forbidden.status).toBe(403);

      const client = new AftRpcClient(fixture.storageDir, fixture.directory);
      const result = await client.call<{ ok: boolean; params: Record<string, unknown> }>("echo", {
        value: 1,
      });
      expect(result).toEqual({ ok: true, params: { value: 1 } });
    } finally {
      server.stop();
    }
  });

  test("client parses legacy integer port files without throwing", async () => {
    // Backward-compat at the parser level: old aft versions wrote a plain integer
    // port file. The new client must still parse those files (without exceptions
    // about JSON parsing or missing token field) and reach the network layer.
    // The current new server WILL reject the resulting tokenless request with 403,
    // which is the intended behavior — but the client should fail at the network
    // layer (HTTP 403), not the file-parsing layer.
    const fixture = makeFixture();
    const server = new AftRpcServer(fixture.storageDir, fixture.directory);
    server.handle("echo", async (params) => ({ params }));

    try {
      const port = await server.start();
      const portPath = rpcPortFilePath(fixture.storageDir, fixture.directory);
      mkdirSync(dirname(portPath), { recursive: true });
      // Overwrite with legacy integer-only format (old aft versions used this).
      writeFileSync(portPath, String(port), "utf-8");

      const client = new AftRpcClient(fixture.storageDir, fixture.directory);
      // The client must reach the server (parse port + connect), not crash on
      // missing JSON token. The 403 here proves the legacy parser path works
      // end-to-end — only the new server's token check rejects, not the client.
      await expect(client.call("echo", {})).rejects.toThrow("403");
    } finally {
      server.stop();
    }
  });

  test("client interoperates with a tokenless legacy server", async () => {
    // True backward-compat test: simulate an OLD aft server that never enforced
    // tokens (pre-#23 behavior). New client must still talk to it successfully.
    // We mock this with a plain http.Server that ignores any token field.
    const fixture = makeFixture();
    const { createServer } = await import("node:http");
    const legacyServer = createServer((req, res) => {
      // Legacy aft servers responded to /health unauthenticated and /rpc/* without
      // checking any token. We mirror that here so the new client's resolvePortInfo
      // path (which health-checks before sending RPC) accepts the legacy server.
      if (req.url === "/health") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true }));
        return;
      }
      let body = "";
      req.on("data", (chunk) => {
        body += chunk;
      });
      req.on("end", () => {
        if (req.url?.startsWith("/rpc/")) {
          // Old server: accept without checking token field.
          const params = JSON.parse(body) as Record<string, unknown>;
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ ok: true, echoed: params }));
          return;
        }
        res.writeHead(404);
        res.end();
      });
    });

    await new Promise<void>((resolve) => legacyServer.listen(0, "127.0.0.1", resolve));
    const address = legacyServer.address();
    const port = typeof address === "object" && address ? address.port : 0;

    try {
      // Write legacy integer-only port file (matches what old plugins wrote).
      const portPath = rpcPortFilePath(fixture.storageDir, fixture.directory);
      mkdirSync(dirname(portPath), { recursive: true });
      writeFileSync(portPath, String(port), "utf-8");

      const client = new AftRpcClient(fixture.storageDir, fixture.directory);
      const result = await client.call<{ ok: boolean; echoed: Record<string, unknown> }>("echo", {
        value: 42,
      });
      expect(result.ok).toBe(true);
      // params include the token field (null), which the legacy server should ignore.
      expect(result.echoed.value).toBe(42);
    } finally {
      await new Promise<void>((resolve) => legacyServer.close(() => resolve()));
    }
  });
});
