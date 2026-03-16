import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import type { ToolContext } from "@opencode-ai/plugin";
import { BridgePool } from "../pool.js";
import { aftPrefixedTools } from "../tools/hoisted.js";
import { readingTools } from "../tools/reading.js";
import { refactoringTools } from "../tools/refactoring.js";
import { safetyTools } from "../tools/safety.js";
import type { PluginContext } from "../types.js";

const BINARY_PATH = resolve(import.meta.dir, "../../../../target/debug/aft");
const PROJECT_CWD = resolve(import.meta.dir, "../../../..");
const FIXTURE_FILE = resolve(PROJECT_CWD, "crates/aft/tests/fixtures/sample.ts");
let sdkCtx = createMockSdkContext(PROJECT_CWD);
const TEST_TIMEOUT_MS = 10_000;

/**
 * Creates a mock client that returns no connected LSP servers.
 * This ensures queryLspHints returns undefined (no-op) during integration tests.
 */
function createMockClient(): any {
  return {
    lsp: {
      status: async () => ({ data: [] }),
    },
    find: {
      symbols: async () => ({ data: [] }),
    },
  };
}

/** Helper to create a PluginContext with a pool and a mock client. */
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

describe("Tool round-trips", () => {
  let pool: BridgePool;
  let tmpDir: string | null = null;

  // Fresh pool per test — each test is independent
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

  test("aft_outline tool returns entries for fixture file with known symbols", async () => {
    createBridge();
    const tools = readingTools(createPluginContext(pool));

    const resultStr = await tools.aft_outline.execute({ file: FIXTURE_FILE }, sdkCtx);
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(Array.isArray(result.entries)).toBe(true);
    expect(result.entries.length).toBeGreaterThan(0);

    // Verify known symbols from the fixture
    const names = result.entries.map((e: any) => e.name);
    expect(names).toContain("greet");
    expect(names).toContain("add");
    expect(names).toContain("UserService");
    expect(names).toContain("Config");
    expect(names).toContain("Status");
    expect(names).toContain("UserId");
    expect(names).toContain("internalHelper");

    // Verify structure of an entry
    const greetEntry = result.entries.find((e: any) => e.name === "greet");
    expect(greetEntry.kind).toBe("function");
    expect(greetEntry.exported).toBe(true);
    expect(greetEntry.range).toBeDefined();
    expect(greetEntry.range.start_line).toBeDefined();
    expect(greetEntry.range.end_line).toBeDefined();
  });

  test("write tool creates a temp file and returns syntax_valid", async () => {
    createBridge();
    const tools = aftPrefixedTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "written.ts");
    const content = 'export function greetWorld(): string {\n  return "hello world";\n}\n';

    const resultStr = await tools.aft_edit.execute(
      {
        mode: "write",
        file: filePath,
        content,
        create_dirs: false,
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.syntax_valid).toBe(true);
    expect(result.file).toBe(filePath);

    // Verify the file was actually written
    const fileContent = await readFile(filePath, "utf-8");
    expect(fileContent).toBe(content);
  });

  test("edit_symbol replaces a function and returns backup_id and syntax_valid", async () => {
    createBridge();
    const tools = aftPrefixedTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "editable.ts");
    const original = 'export function hello(): string {\n  return "hi";\n}\n';

    // First write the file
    await tools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    // Now replace the symbol
    const newContent = 'export function hello(): string {\n  return "world";\n}\n';
    const resultStr = await tools.aft_edit.execute(
      {
        mode: "symbol",
        file: filePath,
        symbol: "hello",
        operation: "replace",
        content: newContent,
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.backup_id).toBeDefined();
    expect(typeof result.backup_id).toBe("string");
    expect(result.symbol).toBe("hello");
    expect(result.operation).toBe("replace");

    // Verify the file was actually changed
    const fileContent = await readFile(filePath, "utf-8");
    expect(fileContent).toContain("world");
    expect(fileContent).not.toContain('"hi"');
  });

  test("undo restores the file after edit_symbol", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    const undoTools = safetyTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "undoable.ts");
    const original =
      "export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n";

    // Write original file
    await editTools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    // Edit the symbol
    const replacement =
      "export function greet(name: string): string {\n  return `Goodbye, ${name}!`;\n}\n";
    const editResult = JSON.parse(
      await editTools.aft_edit.execute(
        {
          mode: "symbol",
          file: filePath,
          symbol: "greet",
          operation: "replace",
          content: replacement,
        },
        sdkCtx,
      ),
    );
    expect(editResult.ok).toBe(true);

    // Verify file was changed
    let content = await readFile(filePath, "utf-8");
    expect(content).toContain("Goodbye");

    // Undo the edit
    const undoResult = JSON.parse(
      await undoTools.aft_safety.execute({ op: "undo", file: filePath }, sdkCtx),
    );
    expect(undoResult.ok).toBe(true);
    expect(undoResult.backup_id).toBeDefined();

    // Verify file was restored
    content = await readFile(filePath, "utf-8");
    expect(content).toContain("Hello");
    expect(content).not.toContain("Goodbye");
  });

  test("write dry_run returns diff without modifying file", async () => {
    createBridge();
    const tools = aftPrefixedTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const filePath = resolve(tmpDir, "dryrun.ts");
    const original = 'export function hello(): string {\n  return "hi";\n}\n';

    // Write the original file first
    await tools.aft_edit.execute({ mode: "write", file: filePath, content: original }, sdkCtx);

    // Now dry-run a write with different content
    const newContent = 'export function hello(): string {\n  return "world";\n}\n';
    const resultStr = await tools.aft_edit.execute(
      {
        mode: "write",
        file: filePath,
        content: newContent,
        dry_run: true,
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.dry_run).toBe(true);
    expect(typeof result.diff).toBe("string");
    expect(result.diff).toContain("-");
    expect(result.diff).toContain("+");
    expect(result.syntax_valid).toBe(true);

    // Verify file was NOT modified
    const fileContent = await readFile(filePath, "utf-8");
    expect(fileContent).toBe(original);
  });

  test("transaction success applies multiple file writes", async () => {
    createBridge();
    const tools = aftPrefixedTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));
    sdkCtx = createMockSdkContext(tmpDir);

    const file1 = resolve(tmpDir, "a.ts");
    const file2 = resolve(tmpDir, "b.ts");

    const resultStr = await tools.aft_edit.execute(
      {
        mode: "transaction",
        operations: [
          { file: file1, command: "write", content: "export const a = 1;\n" },
          { file: file2, command: "write", content: "export const b = 2;\n" },
        ],
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    expect(result.ok).toBe(true);
    expect(result.files_modified).toBe(2);
    expect(Array.isArray(result.results)).toBe(true);
    expect(result.results.length).toBe(2);

    // Verify both files were created
    const content1 = await readFile(file1, "utf-8");
    const content2 = await readFile(file2, "utf-8");
    expect(content1).toContain("a = 1");
    expect(content2).toContain("b = 2");
  });

  test("transaction rollback on syntax error", async () => {
    createBridge();
    const editTools = aftPrefixedTools(createPluginContext(pool));
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));
    sdkCtx = createMockSdkContext(tmpDir);

    // Create a valid file that should be restored on rollback
    const existingFile = resolve(tmpDir, "existing.ts");
    const originalContent = "export const x = 1;\n";
    await editTools.aft_edit.execute(
      { mode: "write", file: existingFile, content: originalContent },
      sdkCtx,
    );

    // Transaction: write valid content to existing file, then write broken syntax to new file
    const brokenFile = resolve(tmpDir, "broken.ts");
    const resultStr = await editTools.aft_edit.execute(
      {
        mode: "transaction",
        operations: [
          { file: existingFile, command: "write", content: "export const x = 999;\n" },
          { file: brokenFile, command: "write", content: "export const {{{broken = ;\n" },
        ],
      },
      sdkCtx,
    );
    const result = JSON.parse(resultStr);

    // Transaction should fail due to syntax error
    expect(result.ok).toBe(false);
    expect(result.code).toBe("transaction_failed");
    expect(Array.isArray(result.rolled_back)).toBe(true);

    // Existing file should be restored to original content
    const restoredContent = await readFile(existingFile, "utf-8");
    expect(restoredContent).toBe(originalContent);
  });
});

describe("move_symbol round-trip", () => {
  let pool: BridgePool;
  let tmpDir: string;

  const TEST_TIMEOUT_MS = 15_000;

  afterEach(async () => {
    pool?.shutdown();
    if (tmpDir) await rm(tmpDir, { recursive: true, force: true });
  });

  test(
    "aft_move_symbol moves a function and rewires consumer import",
    async () => {
      pool = new BridgePool(BINARY_PATH, {
        timeoutMs: TEST_TIMEOUT_MS,
      });

      // Create temp project with source, consumer, and destination
      tmpDir = await mkdtemp(resolve(tmpdir(), "aft-move-"));
      sdkCtx = createMockSdkContext(tmpDir);

      const sourceFile = resolve(tmpDir, "service.ts");
      const consumerFile = resolve(tmpDir, "consumer.ts");
      const destFile = resolve(tmpDir, "utils.ts");

      await writeFile(
        sourceFile,
        [
          "export function formatDate(date: Date): string {",
          "  return date.toISOString();",
          "}",
          "",
          "export function otherFn(): void {}",
          "",
        ].join("\n"),
      );

      await writeFile(
        consumerFile,
        [
          "import { formatDate } from './service';",
          "",
          "export function render(d: Date): string {",
          "  return formatDate(d);",
          "}",
          "",
        ].join("\n"),
      );

      // Move formatDate from service.ts to utils.ts
      const refTools = refactoringTools(createPluginContext(pool));
      const moveResult = JSON.parse(
        await refTools.aft_refactor.execute(
          {
            op: "move",
            file: sourceFile,
            symbol: "formatDate",
            destination: destFile,
          },
          sdkCtx,
        ),
      );

      expect(moveResult.ok).toBe(true);
      expect(moveResult.files_modified).toBeGreaterThanOrEqual(2);

      // Verify response structure includes expected diagnostic fields
      expect(moveResult.consumers_updated).toBeDefined();
      expect(moveResult.checkpoint_name).toBeDefined();

      // Verify symbol was actually moved on disk
      const sourceContent = await readFile(sourceFile, "utf-8");
      expect(sourceContent).not.toContain("formatDate");
      expect(sourceContent).toContain("otherFn");

      const destContent = await readFile(destFile, "utf-8");
      expect(destContent).toContain("formatDate");

      // Verify consumer import was rewired
      const consumerContent = await readFile(consumerFile, "utf-8");
      expect(consumerContent).toContain("./utils");
      expect(consumerContent).not.toContain("./service");
    },
    TEST_TIMEOUT_MS,
  );
});

describe("extract_function round-trip", () => {
  let pool: BridgePool;
  let tmpDir: string;

  const TEST_TIMEOUT_MS = 15_000;

  afterEach(async () => {
    pool?.shutdown();
    if (tmpDir) await rm(tmpDir, { recursive: true, force: true });
  });

  test(
    "aft_extract_function extracts code range into a new function with parameters",
    async () => {
      pool = new BridgePool(BINARY_PATH, {
        timeoutMs: TEST_TIMEOUT_MS,
      });

      tmpDir = await mkdtemp(resolve(tmpdir(), "aft-extract-"));
      sdkCtx = createMockSdkContext(tmpDir);

      const filePath = resolve(tmpDir, "source.ts");
      await writeFile(
        filePath,
        [
          "function processData(items: string[], prefix: string): string {",
          "  const filtered = items.filter(item => item.length > 0);",
          "  const mapped = filtered.map(item => prefix + item);",
          '  const result = mapped.join(", ");',
          "  return result;",
          "}",
          "",
        ].join("\n"),
      );

      // Extract lines 1-3 (the filtering and mapping logic)
      const refTools = refactoringTools(createPluginContext(pool));
      const result = JSON.parse(
        await refTools.aft_refactor.execute(
          {
            op: "extract",
            file: filePath,
            name: "filterAndMap",
            start_line: 1,
            end_line: 4,
            dry_run: true,
          },
          sdkCtx,
        ),
      );

      expect(result.ok).toBe(true);
      expect(result.dry_run).toBe(true);
      expect(Array.isArray(result.parameters)).toBe(true);
      expect(result.parameters.length).toBeGreaterThan(0);
      expect(result.return_type).toBeDefined();
      expect(typeof result.diff).toBe("string");
    },
    TEST_TIMEOUT_MS,
  );
});

describe("inline_symbol round-trip", () => {
  let pool: BridgePool;
  let tmpDir: string;

  const TEST_TIMEOUT_MS = 15_000;

  afterEach(async () => {
    pool?.shutdown();
    if (tmpDir) await rm(tmpDir, { recursive: true, force: true });
  });

  test(
    "aft_inline_symbol inlines a function call and returns substitution info",
    async () => {
      pool = new BridgePool(BINARY_PATH, {
        timeoutMs: TEST_TIMEOUT_MS,
      });

      tmpDir = await mkdtemp(resolve(tmpdir(), "aft-inline-"));
      sdkCtx = createMockSdkContext(tmpDir);

      const filePath = resolve(tmpDir, "source.ts");
      await writeFile(
        filePath,
        [
          "function helper(a: number, b: number): number {",
          "  return a + b;",
          "}",
          "",
          "function main() {",
          "  const result = helper(10, 20);",
          "  console.log(result);",
          "}",
          "",
        ].join("\n"),
      );

      // Inline helper at line 6 (const result = helper(10, 20)) — 1-based
      const refTools = refactoringTools(createPluginContext(pool));
      const result = JSON.parse(
        await refTools.aft_refactor.execute(
          {
            op: "inline",
            file: filePath,
            symbol: "helper",
            call_site_line: 6,
          },
          sdkCtx,
        ),
      );

      expect(result.ok).toBe(true);
      expect(result.symbol).toBe("helper");
      expect(result.call_context).toBe("assignment");
      expect(result.substitutions).toBeGreaterThan(0);

      // Verify file was modified
      const content = await readFile(filePath, "utf-8");
      expect(content).not.toContain("helper(10, 20)");
    },
    TEST_TIMEOUT_MS,
  );
});
