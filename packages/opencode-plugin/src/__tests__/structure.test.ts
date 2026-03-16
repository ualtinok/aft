import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";
import { BridgePool } from "../pool.js";
import { aftPrefixedTools } from "../tools/hoisted.js";
import { structureTools } from "../tools/structure.js";
import type { PluginContext } from "../types.js";

const BINARY_PATH = resolve(import.meta.dir, "../../../../target/debug/aft");
const PROJECT_CWD = resolve(import.meta.dir, "../../../..");
let sdkCtx = createMockSdkContext(PROJECT_CWD);

const TEST_TIMEOUT_MS = 10_000;

/** Creates a mock client that returns no connected LSP servers. */
function createMockClient(): any {
  return {
    lsp: { status: async () => ({ data: [] }) },
    find: { symbols: async () => ({ data: [] }) },
  };
}

/** Helper to create a ToolContext with a mock (no-op LSP) client. */
function createPluginContext(pool: BridgePool): PluginContext {
  return { pool, client: createMockClient() };
}

/** Mock SDK ToolContext for test execute calls. */
function createMockSdkContext(directory: string): ToolContext {
  return {
    sessionID: "test",
    messageID: "test",
    agent: "test",
    directory,
    worktree: directory,
    abort: new AbortController().signal,
    metadata: () => {},
    ask: async () => {},
  };
}

/** Create a fake PluginContext for registration-only tests (no real pool). */
function fakePluginContext(): PluginContext {
  return { pool: {} as BridgePool, client: createMockClient() };
}

describe("Structure tool registrations", () => {
  test("structureTools returns the aft_transform tool definition", () => {
    // Use a dummy pool — we're only checking registration, not execution
    const fakeCtx = fakePluginContext();
    const tools = structureTools(fakeCtx);

    const names = Object.keys(tools);
    expect(names).toContain("aft_transform");
    expect(names.length).toBe(1);
  });

  test("each tool has a description and args", () => {
    const fakeCtx = fakePluginContext();
    const tools = structureTools(fakeCtx);

    for (const [_name, def] of Object.entries(tools)) {
      expect(def.description).toBeTruthy();
      expect(typeof def.description).toBe("string");
      expect(def.args).toBeTruthy();
      expect(typeof def.execute).toBe("function");
    }
  });

  test("aft_transform args include op, file, scope, code, and optional position", () => {
    const fakeCtx = fakePluginContext();
    const tools = structureTools(fakeCtx);
    const args = tools.aft_transform.args;

    expect(args.op).toBeDefined();
    expect(args.file).toBeDefined();
    expect(args.scope).toBeDefined();
    expect(args.code).toBeDefined();
    expect(args.position).toBeDefined();
  });

  test("aft_transform args include target and derives", () => {
    const fakeCtx = fakePluginContext();
    const tools = structureTools(fakeCtx);
    const args = tools.aft_transform.args;

    expect(args.file).toBeDefined();
    expect(args.target).toBeDefined();
    expect(args.derives).toBeDefined();
  });

  test("aft_transform args include field, tag, value", () => {
    const fakeCtx = fakePluginContext();
    const tools = structureTools(fakeCtx);
    const args = tools.aft_transform.args;

    expect(args.file).toBeDefined();
    expect(args.target).toBeDefined();
    expect(args.field).toBeDefined();
    expect(args.tag).toBeDefined();
    expect(args.value).toBeDefined();
  });
});

describe("Structure tool round-trips", () => {
  let pool: BridgePool;
  let tmpDir: string | null = null;

  const createBridge = () => {
    pool = new BridgePool(BINARY_PATH, {
      timeoutMs: TEST_TIMEOUT_MS,
    });
    return pool;
  };

  afterEach(async () => {
    if (pool) {
      pool.shutdown();
    }
    if (tmpDir) {
      await rm(tmpDir, { recursive: true, force: true });
      tmpDir = null;
    }
    sdkCtx = createMockSdkContext(PROJECT_CWD);
  });

  test("add_member inserts a method into a TypeScript class", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "example.ts");
    const original = `export class Greeter {\n  name: string;\n}\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_member",
        file: filePath,
        scope: "Greeter",
        code: "greet() { return 'hello'; }",
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.scope).toBe("Greeter");
    expect(result.backup_id).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("greet()");
  });

  test("add_member with position=first inserts at top of class", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "pos.ts");
    const original = `class Foo {\n  existing() {}\n}\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_member",
        file: filePath,
        scope: "Foo",
        code: "first() {}",
        position: "first",
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);

    const content = await readFile(filePath, "utf-8");
    const firstIdx = content.indexOf("first()");
    const existingIdx = content.indexOf("existing()");
    expect(firstIdx).toBeLessThan(existingIdx);
  });

  test("add_derive adds a derive to a Rust struct", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "example.rs");
    const original = `#[derive(Debug)]\nstruct Foo {\n    x: i32,\n}\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_derive",
        file: filePath,
        target: "Foo",
        derives: ["Clone", "PartialEq"],
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.derives).toContain("Debug");
    expect(result.derives).toContain("Clone");
    expect(result.derives).toContain("PartialEq");

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("Clone");
    expect(content).toContain("PartialEq");
  });

  test("wrap_try_catch wraps a function body", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "wrap.ts");
    const original = `function doWork() {\n  const x = 1;\n  return x;\n}\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "wrap_try_catch",
        file: filePath,
        target: "doWork",
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.backup_id).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("try {");
    expect(content).toContain("catch");
  });

  test("wrap_try_catch with custom catch_body", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "wrap2.ts");
    const original = `function risky() {\n  throw new Error("boom");\n}\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "wrap_try_catch",
        file: filePath,
        target: "risky",
        catch_body: 'console.error("failed:", error);',
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("console.error");
  });

  test("add_decorator inserts a Python decorator", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "example.py");
    const original = `class MyClass:\n    def method(self):\n        pass\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_decorator",
        file: filePath,
        target: "method",
        decorator: "staticmethod",
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.backup_id).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("@staticmethod");
  });

  test("add_struct_tags adds a Go struct tag", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "example.go");
    const original = `package main\n\ntype User struct {\n\tName string\n\tAge  int\n}\n`;
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_struct_tags",
        file: filePath,
        target: "User",
        field: "Name",
        tag: "json",
        value: "name,omitempty",
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.tag_string).toBeDefined();

    const content = await readFile(filePath, "utf-8");
    expect(content).toContain("json");
    expect(content).toContain("name,omitempty");
  });

  test("add_member returns scope_not_found for missing scope", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "missing.ts");
    await editTools.aft_edit.execute(
      {
        mode: "write",
        file: filePath,
        content: `class Real {\n  x: number;\n}\n`,
      },
      sdkCtx,
    );

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_member",
        file: filePath,
        scope: "NonExistent",
        code: "y: string;",
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(false);
    expect(result.code).toBe("scope_not_found");
  });

  test("add_derive returns target_not_found for missing struct", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const tools = structureTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-structure-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "missing.rs");
    await editTools.aft_edit.execute(
      {
        mode: "write",
        file: filePath,
        content: `struct Real {\n    x: i32,\n}\n`,
      },
      sdkCtx,
    );

    const resultStr = await tools.aft_transform.execute(
      {
        op: "add_derive",
        file: filePath,
        target: "Fake",
        derives: ["Clone"],
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(false);
    expect(result.code).toBe("target_not_found");
  });
});
