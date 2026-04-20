import { readdir } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import { fetchUrlToTempFile } from "../shared/url-fetch.js";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";

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
        "Structural outline of source code, documentation files, or remote URLs. For code, returns symbols (functions, classes, types) with line ranges. For Markdown and HTML, returns heading hierarchy. Use this to explore structure before reading specific sections with aft_zoom.\n\n" +
        "Provide exactly ONE of: 'filePath', 'files', 'directory', or 'url'.",
      args: {
        filePath: z.string().optional().describe("Path to a single file to outline"),
        files: z
          .array(z.string())
          .optional()
          .describe("Array of file paths to outline in one call"),
        directory: z
          .string()
          .optional()
          .describe("Directory to outline recursively (200 file limit)"),
        url: z
          .string()
          .optional()
          .describe("HTTP/HTTPS URL of an HTML or Markdown document to fetch and outline"),
      },
      execute: async (args, context): Promise<string> => {
        const filesArg = Array.isArray(args.files) ? (args.files as unknown[]) : undefined;
        const hasFilePath = typeof args.filePath === "string" && args.filePath.length > 0;
        const hasFiles = (filesArg?.length ?? 0) > 0;
        const hasDirectory = typeof args.directory === "string" && args.directory.length > 0;
        const hasUrl = typeof args.url === "string" && args.url.length > 0;

        // Mutual exclusion: exactly one of filePath, files, directory, url
        const provided = [hasFilePath, hasFiles, hasDirectory, hasUrl].filter(Boolean).length;
        if (provided === 0) {
          throw new Error("Provide exactly one of 'filePath', 'files', 'directory', or 'url'");
        }
        if (provided > 1) {
          throw new Error(
            "Provide exactly ONE of 'filePath', 'files', 'directory', or 'url' — not multiple",
          );
        }

        // URL mode: fetch to temp file, then outline the cached copy
        if (hasUrl) {
          const cachedPath = await fetchUrlToTempFile(args.url as string, ctx.storageDir);
          const response = await callBridge(ctx, context, "outline", { file: cachedPath });
          if (response.success === false) {
            throw new Error((response.message as string) || "outline failed");
          }
          return response.text as string;
        }

        // Directory mode: discover source files recursively and batch outline
        // Also auto-detect when filePath is a directory and route here
        let dirArg = typeof args.directory === "string" ? args.directory : undefined;
        if (!dirArg && typeof args.filePath === "string" && !Array.isArray(args.files)) {
          try {
            const { stat } = await import("node:fs/promises");
            const resolved = resolve(context.directory, args.filePath);
            const st = await stat(resolved);
            if (st.isDirectory()) {
              dirArg = args.filePath;
            }
          } catch {
            // Not a directory or doesn't exist — fall through to normal file handling
          }
        }

        if (dirArg) {
          const dirPath = resolve(context.directory, dirArg);
          const files = await discoverSourceFiles(dirPath);
          if (files.length === 0) {
            return JSON.stringify({
              success: false,
              message: `No source files found under ${dirArg}`,
            });
          }
          const response = await callBridge(ctx, context, "outline", { files });
          if (response.success === false) {
            throw new Error((response.message as string) || "outline failed");
          }
          return response.text as string;
        }

        if (Array.isArray(args.files) && args.files.length > 0) {
          const response = await callBridge(ctx, context, "outline", { files: args.files });
          if (response.success === false) {
            throw new Error((response.message as string) || "outline failed");
          }
          return response.text as string;
        }
        const response = await callBridge(ctx, context, "outline", { file: args.filePath });
        if (response.success === false) {
          throw new Error((response.message as string) || "outline failed");
        }
        return response.text as string;
      },
    },

    aft_zoom: {
      description:
        "Inspect code symbols or documentation sections. For code, returns the full source of a symbol with call-graph annotations (what it calls and what calls it). For Markdown and HTML, returns the section content under the given heading.\n\n" +
        "Provide exactly ONE of 'filePath' or 'url'. Pass either 'symbol' for a single lookup or 'symbols' for multiple in one call.",
      args: {
        filePath: z
          .string()
          .optional()
          .describe("Path to file (absolute or relative to project root)"),
        url: z
          .string()
          .optional()
          .describe("HTTP/HTTPS URL of an HTML or Markdown document to fetch and zoom into"),
        symbol: z
          .string()
          .optional()
          .describe("Symbol name for code, or heading text for Markdown/HTML"),
        symbols: z
          .array(z.string())
          .optional()
          .describe("Array of symbol names or heading texts for a single batched call"),
        contextLines: z
          .number()
          .optional()
          .describe("Lines of context before/after the symbol (default: 3)"),
      },
      execute: async (args, context): Promise<string> => {
        const hasFilePath = typeof args.filePath === "string" && args.filePath.length > 0;
        const hasUrl = typeof args.url === "string" && args.url.length > 0;

        if (!hasFilePath && !hasUrl) {
          throw new Error("Provide exactly one of 'filePath' or 'url'");
        }
        if (hasFilePath && hasUrl) {
          throw new Error("Provide exactly ONE of 'filePath' or 'url' — not both");
        }

        // URL mode: fetch to temp file, then zoom into the cached copy
        const file = hasUrl
          ? await fetchUrlToTempFile(args.url as string, ctx.storageDir)
          : (args.filePath as string);

        // Multi-symbol mode: make separate zoom calls in parallel and combine results
        if (Array.isArray(args.symbols) && args.symbols.length > 0) {
          const results = await Promise.all(
            (args.symbols as string[]).map((sym) => {
              const params: Record<string, unknown> = { file, symbol: sym };
              if (args.contextLines !== undefined) params.context_lines = args.contextLines;
              return callBridge(ctx, context, "zoom", params);
            }),
          );
          return JSON.stringify(results);
        }

        // Single symbol mode
        const params: Record<string, unknown> = { file };
        if (typeof args.symbol === "string") params.symbol = args.symbol;
        if (args.contextLines !== undefined) params.context_lines = args.contextLines;

        const data = await callBridge(ctx, context, "zoom", params);
        if (data.success === false) {
          throw new Error((data.message as string) || "zoom failed");
        }
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
