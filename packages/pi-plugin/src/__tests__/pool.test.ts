import { describe, expect, test } from "bun:test";
import { mkdtempSync, realpathSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { BridgePool } from "../pool.js";

describe("Pi BridgePool", () => {
  test("getAnyActiveBridge prefers the bridge for the requested directory", () => {
    const pool = new BridgePool("/tmp/aft", { timeoutMs: 1_000 });
    try {
      const projectA = pool.getBridge("/tmp/project-a");
      const projectB = pool.getBridge("/tmp/project-b");
      (projectA as any).isAlive = () => true;
      (projectB as any).isAlive = () => true;

      expect(pool.getAnyActiveBridge("/tmp/project-b")).toBe(projectB);
    } finally {
      pool.shutdown().catch(() => {});
    }
  });

  test("getAnyActiveBridge falls back to another alive bridge", () => {
    const pool = new BridgePool("/tmp/aft", { timeoutMs: 1_000 });
    try {
      const projectA = pool.getBridge("/tmp/project-a");
      const projectB = pool.getBridge("/tmp/project-b");
      (projectA as any).isAlive = () => true;
      (projectB as any).isAlive = () => false;

      expect(pool.getAnyActiveBridge("/tmp/project-b")).toBe(projectA);
    } finally {
      pool.shutdown().catch(() => {});
    }
  });

  test("trailing slash and backslash normalize to the same key", () => {
    const pool = new BridgePool("/tmp/aft", { timeoutMs: 1_000 });
    try {
      const a = pool.getBridge("C:\\repo\\");
      const b = pool.getBridge("C:\\repo");
      const c = pool.getBridge("/tmp/project/");
      const d = pool.getBridge("/tmp/project");

      expect(a).toBe(b);
      expect(c).toBe(d);
      expect(pool.size).toBe(2);
    } finally {
      pool.shutdown().catch(() => {});
    }
  });

  test("canonical paths collapse trailing slash variants to one bridge", () => {
    const real = realpathSync(mkdtempSync(join(tmpdir(), "aft-pi-pool-")));
    const linkDir = realpathSync(mkdtempSync(join(tmpdir(), "aft-pi-pool-link-")));
    const link = join(linkDir, "link");
    symlinkSync(real, link);

    const pool = new BridgePool("/tmp/aft", { timeoutMs: 1_000 });
    try {
      const a = pool.getBridge(link);
      const b = pool.getBridge(`${link}/`);

      expect(a).toBe(b);
      expect(pool.size).toBe(1);
    } finally {
      pool.shutdown().catch(() => {});
    }
  });
});
