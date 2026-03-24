import { readdir } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

/** File extensions that aft_outline supports via tree-sitter or markdown parser */
const OUTLINE_EXTENSIONS = new Set([
  ".ts",
  ".tsx",
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".rs",
  ".go",
  ".py",
  ".rb",
  ".c",
  ".cpp",
  ".h",
  ".hpp",
  ".cs",
  ".java",
  ".kt",
  ".scala",
  ".swift",
  ".lua",
  ".ex",
  ".exs",
  ".hs",
  ".sol",
  ".nix",
  ".md",
  ".mdx",
  ".css",
  ".html",
  ".json",
  ".yaml",
  ".yml",
  ".sh",
  ".bash",
]);

const z = tool.schema;

/**
 * Tool definitions for code reading commands: outline + zoom.
 */
export function readingTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_outline: {
      description:
        "Get a structural outline of a source file, multiple files, or an entire directory — lists all top-level symbols with their kind, name, line range, and visibility. Use this to understand file/directory structure before editing.\n" +
        "Each entry includes 'name', 'kind' (function/class/struct/heading/etc), 'range', 'signature', and 'members' (nested children like methods in classes or sub-headings in markdown).\n" +
        "For Markdown files (.md, .mdx): returns heading hierarchy — h1/h2/h3 as nested symbols with section ranges covering all content until the next same-level heading.\n\n" +
        "Provide 'filePath', 'files', or 'directory'. Priority: directory > files > filePath. If multiple provided, highest-priority wins.\n" +
        "Directory mode skips commonly ignored directories and dot-prefixed directories.",
      args: {
        filePath: z
          .string()
          .optional()
          .describe(
            "Path to a single file to outline (ignored if 'files' or 'directory' is also provided)",
          ),
        files: z
          .array(z.string())
          .optional()
          .describe(
            "Array of file paths to outline in one call (ignored if 'directory' is also provided)",
          ),
        // The 200-file cap is intentionally only in .describe() — not in the main description —
        // because it's a parameter-level detail. The cap is silent (no warning on truncation).
        directory: z
          .string()
          .optional()
          .describe(
            "Path to a directory — outlines all source files under it recursively (capped at 200 files, takes priority over 'filePath' and 'files')",
          ),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);

        const filesArg = Array.isArray(args.files) ? (args.files as unknown[]) : undefined;
        if (!args.filePath && !filesArg?.length && !args.directory) {
          throw new Error("Provide exactly one of 'filePath', 'files', or 'directory'");
        }

        // Directory mode: discover source files recursively and batch outline
        if (typeof args.directory === "string") {
          const dirPath = resolve(context.directory, args.directory);
          const files = await discoverSourceFiles(dirPath);
          if (files.length === 0) {
            return JSON.stringify({
              success: false,
              message: `No source files found under ${args.directory}`,
            });
          }
          const response = await bridge.send("outline", { files });
          return JSON.stringify(response);
        }

        if (Array.isArray(args.files) && args.files.length > 0) {
          const response = await bridge.send("outline", { files: args.files });
          return JSON.stringify(response);
        }
        const response = await bridge.send("outline", { file: args.filePath });
        return JSON.stringify(response);
      },
    },

    aft_zoom: {
      description: `Inspect code symbols with call-graph annotations. Returns the full source of named symbols with what they call and what calls them.

Use this when you need to understand a specific function, class, or type in detail.

**Modes:**

1. **Inspect symbol** — pass filePath + symbol
   Returns full source + call graph annotations.
   Example: { "filePath": "src/app.ts", "symbol": "handleRequest" }

2. **Inspect multiple symbols** — pass filePath + symbols array
   Returns multiple symbols in one call.
   Example: { "filePath": "src/app.ts", "symbols": ["Config", "createApp"] }

For Markdown files, use heading text as symbol name.

Mode priority: symbols array > single symbol.`,
      args: {
        filePath: z.string().describe("Path to file (absolute or relative to project root)"),
        symbol: z.string().optional().describe("Name of a single symbol to inspect"),
        symbols: z
          .array(z.string())
          .optional()
          .describe("Array of symbol names to inspect in one call"),
        contextLines: z
          .number()
          .optional()
          .describe("Lines of context before/after the symbol (default: 3)"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const file = args.filePath as string;

        // Multi-symbol mode: make separate zoom calls in parallel and combine results
        if (Array.isArray(args.symbols) && args.symbols.length > 0) {
          const results = await Promise.all(
            (args.symbols as string[]).map((sym) => {
              const params: Record<string, unknown> = { file, symbol: sym };
              if (args.contextLines !== undefined) params.context_lines = args.contextLines;
              return bridge.send("zoom", params);
            }),
          );
          return JSON.stringify(results);
        }

        // Single symbol mode
        const params: Record<string, unknown> = { file };
        if (typeof args.symbol === "string") params.symbol = args.symbol;
        if (args.contextLines !== undefined) params.context_lines = args.contextLines;

        const data = await bridge.send("zoom", params);
        return JSON.stringify(data);
      },
    },
  };
}

/** Recursively discover source files under a directory, skipping common noise directories */
const SKIP_DIRS = new Set([
  "node_modules",
  ".git",
  "dist",
  "build",
  "out",
  ".next",
  ".nuxt",
  "target",
  "__pycache__",
  ".venv",
  "venv",
  "vendor",
  ".turbo",
  "coverage",
  ".nyc_output",
  ".cache",
]);

async function discoverSourceFiles(dir: string, maxFiles = 200): Promise<string[]> {
  const files: string[] = [];

  async function walk(current: string): Promise<void> {
    if (files.length >= maxFiles) return;

    let entries: import("node:fs").Dirent[];
    try {
      entries = await readdir(current, { withFileTypes: true });
    } catch {
      return; // permission denied, not a directory, etc.
    }

    for (const entry of entries) {
      if (files.length >= maxFiles) return;

      if (entry.isDirectory()) {
        if (!SKIP_DIRS.has(entry.name) && !entry.name.startsWith(".")) {
          await walk(join(current, entry.name));
        }
      } else if (entry.isFile()) {
        const ext = extname(entry.name).toLowerCase();
        if (OUTLINE_EXTENSIONS.has(ext)) {
          files.push(join(current, entry.name));
        }
      }
    }
  }

  await walk(dir);
  files.sort();
  return files;
}
