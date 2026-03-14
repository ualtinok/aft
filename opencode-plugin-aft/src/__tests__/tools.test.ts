import { describe, test, expect, afterEach } from "bun:test";
import { BinaryBridge } from "../bridge.js";
import { readingTools } from "../tools/reading.js";
import { editingTools } from "../tools/editing.js";
import { safetyTools } from "../tools/safety.js";
import { resolve } from "node:path";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";

const BINARY_PATH = resolve(import.meta.dir, "../../../target/debug/aft");
const PROJECT_CWD = resolve(import.meta.dir, "../../..");
const FIXTURE_FILE = resolve(PROJECT_CWD, "tests/fixtures/sample.ts");

const TEST_TIMEOUT_MS = 10_000;

describe("Tool round-trips", () => {
  let bridge: BinaryBridge;
  let tmpDir: string | null = null;

  // Fresh bridge per test — each test is independent
  const createBridge = () => {
    bridge = new BinaryBridge(BINARY_PATH, PROJECT_CWD, {
      timeoutMs: TEST_TIMEOUT_MS,
    });
    return bridge;
  };

  afterEach(async () => {
    if (bridge) {
      await bridge.shutdown();
    }
    if (tmpDir) {
      await rm(tmpDir, { recursive: true, force: true });
      tmpDir = null;
    }
  });

  test("outline tool returns entries for fixture file with known symbols", async () => {
    createBridge();
    const tools = readingTools(bridge);

    const resultStr = await tools.outline.execute({ file: FIXTURE_FILE });
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
    const tools = editingTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));

    const filePath = resolve(tmpDir, "written.ts");
    const content = 'export function greetWorld(): string {\n  return "hello world";\n}\n';

    const resultStr = await tools.write.execute({
      file: filePath,
      content,
      create_dirs: false,
    });
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
    const tools = editingTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));

    const filePath = resolve(tmpDir, "editable.ts");
    const original = 'export function hello(): string {\n  return "hi";\n}\n';

    // First write the file
    await tools.write.execute({ file: filePath, content: original });

    // Now replace the symbol
    const newContent = 'export function hello(): string {\n  return "world";\n}\n';
    const resultStr = await tools.edit_symbol.execute({
      file: filePath,
      symbol: "hello",
      operation: "replace",
      content: newContent,
    });
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
    const editTools = editingTools(bridge);
    const undoTools = safetyTools(bridge);
    tmpDir = await mkdtemp(resolve(tmpdir(), "aft-test-"));

    const filePath = resolve(tmpDir, "undoable.ts");
    const original = 'export function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n';

    // Write original file
    await editTools.write.execute({ file: filePath, content: original });

    // Edit the symbol
    const replacement = 'export function greet(name: string): string {\n  return `Goodbye, ${name}!`;\n}\n';
    const editResult = JSON.parse(
      await editTools.edit_symbol.execute({
        file: filePath,
        symbol: "greet",
        operation: "replace",
        content: replacement,
      }),
    );
    expect(editResult.ok).toBe(true);

    // Verify file was changed
    let content = await readFile(filePath, "utf-8");
    expect(content).toContain("Goodbye");

    // Undo the edit
    const undoResult = JSON.parse(
      await undoTools.undo.execute({ file: filePath }),
    );
    expect(undoResult.ok).toBe(true);
    expect(undoResult.backup_id).toBeDefined();

    // Verify file was restored
    content = await readFile(filePath, "utf-8");
    expect(content).toContain("Hello");
    expect(content).not.toContain("Goodbye");
  });
});
