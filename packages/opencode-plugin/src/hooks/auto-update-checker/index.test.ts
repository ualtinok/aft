import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";

const logMock = mock(() => {});
const warnMock = mock(() => {});

const checkerMocks = {
  extractChannel: mock(() => "latest"),
  findPluginEntry: mock(() => null),
  getCachedVersion: mock(() => null),
  getCurrentRuntimePackageJsonPath: mock(() => null),
  getLatestVersion: mock(async () => null),
  getLocalDevVersion: mock(() => null),
};

const cacheMocks = {
  preparePackageUpdate: mock(() => "/tmp/opencode"),
  resolveInstallContext: mock(() => ({ installDir: "/tmp/opencode" })),
  runBunInstallSafe: mock(async () => true),
};

mock.module("../../logger.js", () => ({
  log: logMock,
  warn: warnMock,
  error: mock(() => {}),
}));

mock.module("./checker.js", () => checkerMocks);
mock.module("./cache.js", () => cacheMocks);

let importCounter = 0;

function freshIndexImport() {
  return import(`./index.ts?test=${importCounter++}`);
}

function createCtx() {
  const showToast = mock(() => Promise.resolve(undefined));
  return {
    ctx: {
      directory: "/test",
      client: { tui: { showToast } },
    },
    showToast,
  };
}

async function waitForCalls(fn: { mock: { calls: unknown[] } }, minCalls = 1): Promise<void> {
  const deadline = Date.now() + 1000;

  while (fn.mock.calls.length < minCalls) {
    if (Date.now() > deadline) throw new Error("Timed out waiting for async hook work");
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

describe("auto-update-checker/index", () => {
  beforeEach(() => {
    logMock.mockClear();
    warnMock.mockClear();

    checkerMocks.extractChannel.mockReset();
    checkerMocks.extractChannel.mockImplementation(() => "latest");
    checkerMocks.findPluginEntry.mockReset();
    checkerMocks.findPluginEntry.mockImplementation(() => null);
    checkerMocks.getCachedVersion.mockReset();
    checkerMocks.getCachedVersion.mockImplementation(() => null);
    checkerMocks.getCurrentRuntimePackageJsonPath.mockReset();
    checkerMocks.getCurrentRuntimePackageJsonPath.mockImplementation(() => null);
    checkerMocks.getLatestVersion.mockReset();
    checkerMocks.getLatestVersion.mockImplementation(async () => null);
    checkerMocks.getLocalDevVersion.mockReset();
    checkerMocks.getLocalDevVersion.mockImplementation(() => null);

    cacheMocks.preparePackageUpdate.mockReset();
    cacheMocks.preparePackageUpdate.mockImplementation(() => "/tmp/opencode");
    cacheMocks.resolveInstallContext.mockReset();
    cacheMocks.resolveInstallContext.mockImplementation(() => ({ installDir: "/tmp/opencode" }));
    cacheMocks.runBunInstallSafe.mockReset();
    cacheMocks.runBunInstallSafe.mockImplementation(async () => true);
  });

  afterEach(() => {
    mock.restore();
  });

  test("uses resolved install root for auto-update installs", async () => {
    const { getAutoUpdateInstallDir } = await freshIndexImport();

    expect(getAutoUpdateInstallDir()).toBe("/tmp/opencode");
  });

  test("shows development toast and skips background update for local dev installs", async () => {
    checkerMocks.getLocalDevVersion.mockImplementation(() => "0.17.2-dev");
    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();

    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
    );
    await hook({ event: { type: "session.created", properties: {} } });
    await waitForCalls(showToast);

    expect(showToast).toHaveBeenCalledWith({
      body: {
        title: "AFT 0.17.2-dev (dev)",
        message: "Running in local development mode.",
        variant: "info",
        duration: 3000,
      },
    });
    expect(checkerMocks.findPluginEntry).not.toHaveBeenCalled();
    expect(checkerMocks.getLatestVersion).not.toHaveBeenCalled();
  });

  test("runs once for root session and ignores child sessions", async () => {
    checkerMocks.getLocalDevVersion.mockImplementation(() => "0.17.2-dev");
    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();
    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
    );

    await hook({
      event: { type: "session.created", properties: { info: { parentID: "parent" } } },
    });
    expect(showToast).not.toHaveBeenCalled();

    await hook({ event: { type: "session.created", properties: { info: {} } } });
    await waitForCalls(showToast);
    await hook({ event: { type: "session.created", properties: { info: {} } } });

    expect(showToast).toHaveBeenCalledTimes(1);
  });

  test("shows success toast after updating the active install root", async () => {
    checkerMocks.findPluginEntry.mockImplementation(() => ({
      entry: "@cortexkit/aft-opencode@latest",
      pinnedVersion: null,
      isPinned: false,
      configPath: "/config/opencode.jsonc",
    }));
    checkerMocks.getCachedVersion.mockImplementation(() => "0.17.1");
    checkerMocks.getLatestVersion.mockImplementation(async () => "0.17.2");

    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();
    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
      {
        showStartupToast: false,
      },
    );

    await hook({ event: { type: "session.created", properties: {} } });
    await waitForCalls(showToast);

    expect(cacheMocks.preparePackageUpdate).toHaveBeenCalledWith(
      "0.17.2",
      "@cortexkit/aft-opencode",
    );
    expect(cacheMocks.runBunInstallSafe).toHaveBeenCalledWith(
      "/tmp/opencode",
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
    expect(showToast).toHaveBeenCalledWith({
      body: {
        title: "AFT Updated!",
        message: "v0.17.1 → v0.17.2\nRestart OpenCode to apply.",
        variant: "success",
        duration: 8000,
      },
    });
  });

  test("shows notification-only toast when auto-update is disabled", async () => {
    checkerMocks.findPluginEntry.mockImplementation(() => ({
      entry: "@cortexkit/aft-opencode@latest",
      pinnedVersion: null,
      isPinned: false,
      configPath: "/config/opencode.jsonc",
    }));
    checkerMocks.getCachedVersion.mockImplementation(() => "0.17.1");
    checkerMocks.getLatestVersion.mockImplementation(async () => "0.17.2");
    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();

    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
      {
        showStartupToast: false,
        autoUpdate: false,
      },
    );
    await hook({ event: { type: "session.created", properties: {} } });
    await waitForCalls(showToast);

    expect(showToast).toHaveBeenCalledWith({
      body: {
        title: "AFT 0.17.2",
        message: "v0.17.2 available. Auto-update is disabled.",
        variant: "info",
        duration: 8000,
      },
    });
    expect(cacheMocks.preparePackageUpdate).not.toHaveBeenCalled();
    expect(cacheMocks.runBunInstallSafe).not.toHaveBeenCalled();
  });

  test("shows pinned-version notification without installing", async () => {
    checkerMocks.findPluginEntry.mockImplementation(() => ({
      entry: "@cortexkit/aft-opencode@0.17.1",
      pinnedVersion: "0.17.1",
      isPinned: true,
      configPath: "/config/opencode.jsonc",
    }));
    checkerMocks.getCachedVersion.mockImplementation(() => "0.17.1");
    checkerMocks.getLatestVersion.mockImplementation(async () => "0.17.2");
    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();

    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
      {
        showStartupToast: false,
      },
    );
    await hook({ event: { type: "session.created", properties: {} } });
    await waitForCalls(showToast);

    expect(showToast).toHaveBeenCalledWith({
      body: {
        title: "AFT 0.17.2",
        message:
          "v0.17.2 available. Version is pinned; update your OpenCode plugin config to upgrade.",
        variant: "info",
        duration: 8000,
      },
    });
    expect(cacheMocks.preparePackageUpdate).not.toHaveBeenCalled();
  });

  test("shows warning toast when latest version fetch fails", async () => {
    checkerMocks.findPluginEntry.mockImplementation(() => ({
      entry: "@cortexkit/aft-opencode@latest",
      pinnedVersion: null,
      isPinned: false,
      configPath: "/config/opencode.jsonc",
    }));
    checkerMocks.getCachedVersion.mockImplementation(() => "0.17.1");
    checkerMocks.getLatestVersion.mockImplementation(async () => null);
    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();

    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
      {
        showStartupToast: false,
      },
    );
    await hook({ event: { type: "session.created", properties: {} } });
    await waitForCalls(showToast);

    expect(showToast).toHaveBeenCalledWith({
      body: {
        title: "AFT update check failed",
        message:
          "Could not check npm for @cortexkit/aft-opencode updates. Continuing with the cached version.",
        variant: "warning",
        duration: 8000,
      },
    });
  });

  test("shows install failure toast without telling users to restart", async () => {
    checkerMocks.findPluginEntry.mockImplementation(() => ({
      entry: "@cortexkit/aft-opencode@latest",
      pinnedVersion: null,
      isPinned: false,
      configPath: "/config/opencode.jsonc",
    }));
    checkerMocks.getCachedVersion.mockImplementation(() => "0.17.1");
    checkerMocks.getLatestVersion.mockImplementation(async () => "0.17.2");
    cacheMocks.runBunInstallSafe.mockImplementation(async () => false);
    const { createAutoUpdateCheckerHook } = await freshIndexImport();
    const { ctx, showToast } = createCtx();

    const hook = createAutoUpdateCheckerHook(
      ctx as Parameters<typeof createAutoUpdateCheckerHook>[0],
      {
        showStartupToast: false,
      },
    );
    await hook({ event: { type: "session.created", properties: {} } });
    await waitForCalls(showToast);

    expect(showToast).toHaveBeenCalledWith({
      body: {
        title: "AFT 0.17.2",
        message:
          "v0.17.2 available, but auto-update failed to install it. Check logs or retry manually.",
        variant: "error",
        duration: 8000,
      },
    });
  });
});
