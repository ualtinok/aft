import { afterEach, describe, expect, mock, spyOn, test } from "bun:test";
import * as childProcess from "node:child_process";
import { EventEmitter } from "node:events";
import * as fs from "node:fs";

mock.module("../../logger.js", () => ({
  log: mock(() => {}),
  warn: mock(() => {}),
  error: mock(() => {}),
}));

let importCounter = 0;

function freshCacheImport() {
  return import(`./cache.ts?test=${importCounter++}`);
}

afterEach(() => {
  mock.restore();
});

describe("auto-update-checker/cache", () => {
  describe("resolveInstallContext", () => {
    test("detects OpenCode packages install root from runtime package path", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockImplementation(
        (p: fs.PathLike) =>
          String(p) ===
          "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest/package.json",
      );
      const { resolveInstallContext } = await freshCacheImport();

      expect(
        resolveInstallContext(
          "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest/node_modules/@cortexkit/aft-opencode/package.json",
        ),
      ).toEqual({
        installDir: "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest",
        packageJsonPath:
          "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest/package.json",
      });

      existsSpy.mockRestore();
    });

    test("does not fall back when runtime path exists but wrapper root is invalid", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockReturnValue(false);
      const { resolveInstallContext } = await freshCacheImport();

      expect(
        resolveInstallContext(
          "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest/node_modules/@cortexkit/aft-opencode/package.json",
        ),
      ).toBeNull();

      existsSpy.mockRestore();
    });
  });

  describe("preparePackageUpdate", () => {
    test("returns null when no install context is available", async () => {
      const existsSpy = spyOn(fs, "existsSync").mockReturnValue(false);
      const { preparePackageUpdate } = await freshCacheImport();

      expect(preparePackageUpdate("0.17.2", "@cortexkit/aft-opencode", null)).toBeNull();

      existsSpy.mockRestore();
    });

    test("updates wrapper dependency and removes installed scoped package", async () => {
      const root = "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest";
      const existsSpy = spyOn(fs, "existsSync").mockImplementation((p: fs.PathLike) => {
        const value = String(p);
        return (
          value === `${root}/package.json` ||
          value === `${root}/node_modules/@cortexkit/aft-opencode`
        );
      });
      const readSpy = spyOn(fs, "readFileSync").mockImplementation((p: fs.PathOrFileDescriptor) => {
        if (String(p) === `${root}/package.json`) {
          return JSON.stringify({ dependencies: { "@cortexkit/aft-opencode": "0.17.1" } });
        }
        return "";
      });
      const writes: string[] = [];
      const writeSpy = spyOn(fs, "writeFileSync").mockImplementation(
        (_path: fs.PathOrFileDescriptor, data: string | NodeJS.ArrayBufferView) => {
          writes.push(String(data));
        },
      );
      const rmSpy = spyOn(fs, "rmSync").mockReturnValue(undefined);
      const { preparePackageUpdate } = await freshCacheImport();

      expect(
        preparePackageUpdate(
          "0.17.2",
          "@cortexkit/aft-opencode",
          `${root}/node_modules/@cortexkit/aft-opencode/package.json`,
        ),
      ).toBe(root);
      expect(JSON.parse(writes[0])).toEqual({
        dependencies: { "@cortexkit/aft-opencode": "0.17.2" },
      });
      expect(rmSpy).toHaveBeenCalledWith(`${root}/node_modules/@cortexkit/aft-opencode`, {
        recursive: true,
        force: true,
      });

      existsSpy.mockRestore();
      readSpy.mockRestore();
      writeSpy.mockRestore();
      rmSpy.mockRestore();
    });

    test("does not rewrite package.json when dependency is already target version", async () => {
      const root = "/home/user/.cache/opencode/packages/@cortexkit/aft-opencode@latest";
      const existsSpy = spyOn(fs, "existsSync").mockImplementation((p: fs.PathLike) => {
        const value = String(p);
        return (
          value === `${root}/package.json` ||
          value === `${root}/node_modules/@cortexkit/aft-opencode`
        );
      });
      const readSpy = spyOn(fs, "readFileSync").mockReturnValue(
        JSON.stringify({ dependencies: { "@cortexkit/aft-opencode": "0.17.2" } }),
      );
      const writeSpy = spyOn(fs, "writeFileSync").mockImplementation(() => {});
      const rmSpy = spyOn(fs, "rmSync").mockReturnValue(undefined);
      const { preparePackageUpdate } = await freshCacheImport();

      expect(
        preparePackageUpdate(
          "0.17.2",
          "@cortexkit/aft-opencode",
          `${root}/node_modules/@cortexkit/aft-opencode/package.json`,
        ),
      ).toBe(root);
      expect(writeSpy).not.toHaveBeenCalled();
      expect(rmSpy).toHaveBeenCalled();

      existsSpy.mockRestore();
      readSpy.mockRestore();
      writeSpy.mockRestore();
      rmSpy.mockRestore();
    });
  });

  describe("runBunInstallSafe", () => {
    test("returns true for successful bun install", async () => {
      const proc = new EventEmitter();
      const spawnMock = spyOn(childProcess, "spawn").mockImplementation(() => {
        setTimeout(() => proc.emit("exit", 0), 0);
        return proc as childProcess.ChildProcess;
      });
      const { runBunInstallSafe } = await freshCacheImport();

      expect(await runBunInstallSafe("/tmp/opencode", { timeoutMs: 1000 })).toBe(true);
      expect(spawnMock).toHaveBeenCalledWith("bun", ["install"], {
        cwd: "/tmp/opencode",
        stdio: "pipe",
      });

      spawnMock.mockRestore();
    });

    test("kills install process and returns false on timeout", async () => {
      const proc = new EventEmitter() as childProcess.ChildProcess;
      const killMock = mock(() => true);
      proc.kill = killMock;
      const spawnMock = spyOn(childProcess, "spawn").mockReturnValue(proc);
      const { runBunInstallSafe } = await freshCacheImport();

      expect(await runBunInstallSafe("/tmp/opencode", { timeoutMs: 1 })).toBe(false);
      expect(killMock).toHaveBeenCalled();

      spawnMock.mockRestore();
    });
  });
});
