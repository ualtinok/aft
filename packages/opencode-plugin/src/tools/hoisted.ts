/**
 * Hoisted tools that replace opencode's built-in tools (read, write, edit, apply_patch).
 *
 * When hoist_builtin_tools is enabled (default), these tools are registered with
 * the SAME names as opencode's built-in tools, effectively overriding them.
 * When disabled, they're registered with aft_ prefix (e.g., aft_read).
 *
 * All file operations go through AFT's Rust binary for better performance,
 * backup tracking, formatting, and inline diagnostics.
 */
import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { parsePatch, applyUpdateChunks } from "../patch-parser.js";
import * as path from "node:path";
import * as fs from "node:fs";

const z = tool.schema;

// ---------------------------------------------------------------------------
// Descriptions — verbose because .describe() on Zod args does NOT reach the agent.
// The description string is the ONLY documentation the LLM sees.
// ---------------------------------------------------------------------------

const READ_DESCRIPTION = `Read files, directories, or inspect code symbols with call-graph annotations.

**Modes** (determined by which parameters you provide):

1. **Read file** (default) — pass \`file\` only
   Returns line-numbered content. Use \`start_line\`/\`end_line\` to read specific sections.
   Example: \`{ "file": "src/app.ts" }\` or \`{ "file": "src/app.ts", "start_line": 50, "end_line": 100 }\`

2. **Inspect symbol** — pass \`file\` + \`symbol\`
   Returns the full source of a named symbol (function, class, type) with call-graph
   annotations showing what it calls and what calls it. Includes surrounding context lines.
   Example: \`{ "file": "src/app.ts", "symbol": "handleRequest" }\`

3. **Inspect multiple symbols** — pass \`file\` + \`symbols\` array
   Returns multiple symbols in one call. More efficient than separate calls.
   Example: \`{ "file": "src/app.ts", "symbols": ["Config", "createApp"] }\`

4. **List directory** — pass \`file\` pointing to a directory
   Returns sorted entries, directories have trailing \`/\`.
   Example: \`{ "file": "src/" }\`

**Parameters:**
- \`file\` (string, required): Path to file or directory (absolute or relative to project root)
- \`symbol\` (string): Name of a single symbol to inspect — returns full source + call graph
- \`symbols\` (string[]): Array of symbol names to inspect in one call
- \`start_line\` (number): 1-based line to start reading from (default: 1)
- \`end_line\` (number): 1-based line to stop reading at, inclusive
- \`limit\` (number): Max lines to return (default: 2000). Ignored when end_line is set.
- \`context_lines\` (number): Lines of context around symbols (default: 3)

**Behavior:**
- Lines longer than 2000 characters are truncated
- Output capped at 50KB — use start_line/end_line to page through large files
- Binary files are auto-detected and return a size-only message
- Symbol mode includes \`calls_out\` and \`called_by\` annotations from call-graph analysis
- For Markdown files, use heading text as symbol name (e.g., symbol: "Architecture")`;

/**
 * Creates the unified read tool. Registers as "read" when hoisted, "aft_read" when not.
 */
export function createReadTool(ctx: PluginContext): ToolDefinition {
  return {
    description: READ_DESCRIPTION,
    args: {
      file: z.string(),
      symbol: z.string().optional(),
      symbols: z.array(z.string()).optional(),
      start_line: z.number().optional(),
      end_line: z.number().optional(),
      limit: z.number().optional(),
      context_lines: z.number().optional(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const file = args.file as string;

      // Resolve relative paths
      const filePath = path.isAbsolute(file)
        ? file
        : path.resolve(context.directory, file);

      // Permission check
      await context.ask({
        permission: "read",
        patterns: [filePath],
        always: ["*"],
        metadata: {},
      });

      // Image/PDF detection — return metadata for UI preview
      const ext = path.extname(filePath).toLowerCase();
      const mimeMap: Record<string, string> = {
        ".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
        ".gif": "image/gif", ".webp": "image/webp", ".bmp": "image/bmp",
        ".ico": "image/x-icon", ".tiff": "image/tiff", ".tif": "image/tiff",
        ".avif": "image/avif", ".heic": "image/heic", ".heif": "image/heif",
        ".pdf": "application/pdf",
      };
      const mime = mimeMap[ext];
      if (mime) {
        const isImage = mime.startsWith("image/");
        const label = isImage ? "Image" : "PDF";
        let fileSize = 0;
        try {
          const stat = await import("node:fs/promises").then(fs => fs.stat(filePath));
          fileSize = stat.size;
        } catch { /* ignore */ }
        const sizeStr = fileSize > 1024 * 1024
          ? `${(fileSize / (1024 * 1024)).toFixed(1)}MB`
          : fileSize > 1024
            ? `${(fileSize / 1024).toFixed(0)}KB`
            : `${fileSize} bytes`;
        const msg = `${label} read successfully`;
        context.metadata({
          title: path.relative(context.worktree, filePath),
          metadata: {
            preview: msg,
            filepath: filePath,
            isImage,
            isPdf: mime === "application/pdf",
          },
        });
        return `${msg} (${ext.slice(1).toUpperCase()}, ${sizeStr}). File: ${filePath}`;
      }

      // Route: symbol/symbols → zoom command, everything else → read command
      const hasSymbol = typeof args.symbol === "string" && args.symbol.length > 0;
      const hasSymbols = Array.isArray(args.symbols) && args.symbols.length > 0;

      if (hasSymbol || hasSymbols) {
        // Symbol mode → zoom command
        const params: Record<string, unknown> = { file: filePath };
        if (hasSymbol) params.symbol = args.symbol;
        if (hasSymbols) params.symbols = args.symbols;
        if (args.start_line !== undefined) params.start_line = args.start_line;
        if (args.end_line !== undefined) params.end_line = args.end_line;
        if (args.context_lines !== undefined) params.context_lines = args.context_lines;

        const data = await bridge.send("zoom", params);
        return JSON.stringify(data);
      }

      // Line-range mode with start_line + end_line → also zoom (has context_before/after)
      if (args.start_line !== undefined && args.end_line !== undefined) {
        const params: Record<string, unknown> = {
          file: filePath,
          start_line: args.start_line,
          end_line: args.end_line,
        };
        if (args.context_lines !== undefined) params.context_lines = args.context_lines;

        const data = await bridge.send("zoom", params);
        return JSON.stringify(data);
      }

      // Plain read mode → read command (line-numbered, truncated, binary/dir detection)
      const params: Record<string, unknown> = { file: filePath };
      if (args.start_line !== undefined) params.start_line = args.start_line;
      if (args.end_line !== undefined) params.end_line = args.end_line;
      if (args.limit !== undefined) params.limit = args.limit;

      const data = await bridge.send("read", params);

      // Directory response
      if (data.entries) {
        return (data.entries as string[]).join("\n");
      }

      // Binary response
      if (data.binary) {
        return data.message as string;
      }

      // File content — already line-numbered from Rust
      let output = data.content as string;

      // Add navigation hint if truncated
      if (data.truncated) {
        output += `\n(Showing lines ${data.start_line}-${data.end_line} of ${data.total_lines}. Use start_line/end_line to read other sections.)`;
      }

      return output;
    },
  };
}

// ---------------------------------------------------------------------------
// WRITE tool
// ---------------------------------------------------------------------------

const WRITE_DESCRIPTION = `Write content to a file, creating it (and parent directories) if needed.

Automatically creates parent directories. Backs up existing files before overwriting.
If the project has a formatter configured (biome, prettier, rustfmt, etc.), the file
is auto-formatted after writing. Returns inline LSP diagnostics when available.

**Parameters:**
- \`file\` (string, required): Path to the file to write (absolute or relative to project root)
- \`content\` (string, required): The full content to write to the file

**Behavior:**
- Creates parent directories automatically (no need to mkdir first)
- Existing files are backed up before overwriting (recoverable via aft_safety undo)
- Auto-formats using project formatter if configured (biome.json, .prettierrc, etc.)
- Returns LSP diagnostics inline if type errors are introduced
- Use this for creating new files or completely replacing file contents
- For partial edits (find/replace), use the \`edit\` tool instead`;

function createWriteTool(ctx: PluginContext): ToolDefinition {
  return {
    description: WRITE_DESCRIPTION,
    args: {
      file: z.string(),
      content: z.string(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const file = args.file as string;
      const content = args.content as string;

      const filePath = path.isAbsolute(file)
        ? file
        : path.resolve(context.directory, file);

      // Permission check
      await context.ask({
        permission: "edit",
        patterns: [path.relative(context.worktree, filePath)],
        always: ["*"],
        metadata: { filepath: filePath },
      });

      const data = await bridge.send("write", {
        file: filePath,
        content,
        create_dirs: true,
        diagnostics: true,
      });

      let output = data.created ? "Created new file." : "File updated.";
      if (data.formatted) output += " Auto-formatted.";

      // Append inline diagnostics if present
      const diags = data.lsp_diagnostics as Array<Record<string, unknown>> | undefined;
      if (diags && diags.length > 0) {
        const errors = diags.filter((d) => d.severity === "error");
        if (errors.length > 0) {
          output += "\n\nLSP errors detected, please fix:\n";
          for (const d of errors) {
            output += `  Line ${d.line}: ${d.message}\n`;
          }
        }
      }

      return output;
    },
  };
}

// ---------------------------------------------------------------------------
// EDIT tool
// ---------------------------------------------------------------------------

const EDIT_DESCRIPTION = `Edit a file by finding and replacing text, or by targeting named symbols.

**Modes** (determined by which parameters you provide):

1. **Find and replace** — pass \`file\` + \`match\` + \`replacement\`
   Finds the exact text in \`match\` and replaces it with \`replacement\`.
   Returns an error if multiple matches are found (use \`occurrence\` to select one,
   or \`replace_all: true\` to replace all).
   Example: \`{ "file": "src/app.ts", "match": "const x = 1", "replacement": "const x = 2" }\`

2. **Replace all occurrences** — add \`replace_all: true\`
   Replaces every occurrence of \`match\` in the file.
   Example: \`{ "file": "src/app.ts", "match": "oldName", "replacement": "newName", "replace_all": true }\`

3. **Select specific occurrence** — add \`occurrence: N\` (0-indexed)
   When multiple matches exist, select the Nth one (0 = first, 1 = second, etc.).
   Example: \`{ "file": "src/app.ts", "match": "TODO", "replacement": "DONE", "occurrence": 0 }\`

4. **Symbol replace** — pass \`file\` + \`symbol\` + \`content\`
   Replaces an entire named symbol (function, class, type) with new content.
   Includes decorators, attributes, and doc comments in the replacement range.
   Example: \`{ "file": "src/app.ts", "symbol": "handleRequest", "content": "function handleRequest() { ... }" }\`

5. **Batch edits** — pass \`file\` + \`edits\` array
   Multiple edits in one file atomically. Each edit is either:
   - \`{ "match": "old", "replacement": "new" }\` — find/replace
   - \`{ "line_start": 5, "line_end": 7, "content": "new lines" }\` — replace line range (1-based, inclusive)
   Set content to empty string to delete lines.
   Example: \`{ "file": "src/app.ts", "edits": [{ "match": "foo", "replacement": "bar" }, { "line_start": 10, "line_end": 12, "content": "" }] }\`

6. **Multi-file transaction** — pass \`operations\` array
   Atomic edits across multiple files with rollback on failure.
   Example: \`{ "operations": [{ "file": "a.ts", "command": "write", "content": "..." }, { "file": "b.ts", "command": "edit_match", "match": "x", "replacement": "y" }] }\`

7. **Glob replace** — pass \`file\` as glob pattern (e.g. \`"src/**/*.ts"\`) + \`match\` + \`replacement\`
   Replaces across all matching files. Must use \`replace_all: true\`.
   Example: \`{ "file": "src/**/*.ts", "match": "@deprecated", "replacement": "", "replace_all": true }\`

**Parameters:**
- \`file\` (string): Path to file, or glob pattern for multi-file operations
- \`match\` (string): Text to find (exact match). For multi-line, use actual newlines.
- \`replacement\` (string): Text to replace with
- \`replace_all\` (boolean): Replace all occurrences instead of erroring on ambiguity
- \`occurrence\` (number): 0-indexed occurrence to replace when multiple matches exist
- \`symbol\` (string): Named symbol to replace (function, class, type)
- \`content\` (string): New content for symbol replace or file write
- \`edits\` (array): Batch edits — array of { match, replacement } or { line_start, line_end, content }
- \`operations\` (array): Transaction — array of { file, command, ... } for atomic multi-file edits
- \`dry_run\` (boolean): Preview changes without applying (returns diff)
- \`diagnostics\` (boolean): Return inline LSP diagnostics after the edit

**Behavior:**
- Backs up files before editing (recoverable via aft_safety undo)
- Auto-formats using project formatter if configured
- Tree-sitter syntax validation on all edits
- Symbol replace includes decorators, attributes, and doc comments in range`;

function createEditTool(ctx: PluginContext): ToolDefinition {
  return {
    description: EDIT_DESCRIPTION,
    args: {
      file: z.string().optional(),
      match: z.string().optional(),
      replacement: z.string().optional(),
      replace_all: z.boolean().optional(),
      occurrence: z.number().optional(),
      symbol: z.string().optional(),
      content: z.string().optional(),
      edits: z.array(z.record(z.string(), z.unknown())).optional(),
      operations: z.array(z.record(z.string(), z.unknown())).optional(),
      dry_run: z.boolean().optional(),
      diagnostics: z.boolean().optional(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);

      // Transaction mode — multi-file
      if (Array.isArray(args.operations)) {
        const ops = args.operations as Array<Record<string, unknown>>;
        const files = ops.map((op) => op.file as string).filter(Boolean);

        await context.ask({
          permission: "edit",
          patterns: files.map((f) => path.relative(context.worktree, path.resolve(context.directory, f))),
          always: ["*"],
          metadata: {},
        });

        const resolvedOps = ops.map((op) => ({
          ...op,
          file: path.isAbsolute(op.file as string)
            ? op.file
            : path.resolve(context.directory, op.file as string),
        }));

        const data = await bridge.send("transaction", { operations: resolvedOps });
        return JSON.stringify(data);
      }

      const file = args.file as string;
      if (!file) throw new Error("'file' parameter is required");

      const filePath = path.isAbsolute(file)
        ? file
        : path.resolve(context.directory, file);

      await context.ask({
        permission: "edit",
        patterns: [path.relative(context.worktree, filePath)],
        always: ["*"],
        metadata: { filepath: filePath },
      });

      const params: Record<string, unknown> = { file: filePath };

      // Route to appropriate Rust command
      let command: string;

      if (Array.isArray(args.edits)) {
        // Batch mode
        command = "batch";
        params.edits = args.edits;
      } else if (typeof args.symbol === "string") {
        // Symbol replace
        command = "edit_symbol";
        params.symbol = args.symbol;
        params.operation = "replace";
        if (args.content !== undefined) params.content = args.content;
      } else if (typeof args.match === "string") {
        // Find/replace mode (including glob)
        command = "edit_match";
        params.match = args.match;
        if (args.replacement !== undefined) params.replacement = args.replacement;
        if (args.replace_all !== undefined) params.replace_all = args.replace_all;
        if (args.occurrence !== undefined) params.occurrence = args.occurrence;
      } else if (typeof args.content === "string") {
        // Write mode (full file content)
        command = "write";
        params.content = args.content;
        params.create_dirs = true;
      } else {
        throw new Error("Provide 'match' + 'replacement', 'symbol' + 'content', 'edits' array, or 'content' for write");
      }

      if (args.dry_run) params.dry_run = true;
      if (args.diagnostics) params.diagnostics = true;

      const data = await bridge.send(command, params);
      return JSON.stringify(data);
    },
  };
}

// ---------------------------------------------------------------------------
// APPLY_PATCH tool
// ---------------------------------------------------------------------------

const APPLY_PATCH_DESCRIPTION = `Apply a multi-file patch to create, update, delete, or move files in one operation.

Uses the opencode patch format with \`*** Begin Patch\` / \`*** End Patch\` markers.

**Patch format:**
\`\`\`
*** Begin Patch
*** Add File: path/to/new-file.ts
+line 1 of new file
+line 2 of new file
*** Update File: path/to/existing-file.ts
@@ function targetFunction()
-old line to remove
+new line to add
 context line (unchanged, prefixed with space)
*** Delete File: path/to/obsolete-file.ts
*** End Patch
\`\`\`

**File operations:**
- \`*** Add File: <path>\` — Create a new file. Every line prefixed with \`+\`.
- \`*** Update File: <path>\` — Patch an existing file. Uses \`@@\` context anchors.
- \`*** Delete File: <path>\` — Remove a file.
- \`*** Move to: <path>\` — After Update File header, renames the file.

**Update file syntax:**
- \`@@ context line\` — Anchor: finds this line in the file to locate the edit
- \`-line\` — Remove this line
- \`+line\` — Add this line
- \` line\` — Context line (space prefix), appears in both old and new

**Parameters:**
- \`patch\` (string, required): The full patch text including Begin/End markers

**Behavior:**
- All file changes are applied atomically — if any file fails, all changes are rolled back
- Files are backed up before modification
- Parent directories are created automatically for new files
- Fuzzy matching for context anchors (handles whitespace and Unicode differences)`;

function createApplyPatchTool(ctx: PluginContext): ToolDefinition {
  return {
    description: APPLY_PATCH_DESCRIPTION,
    args: {
      patch: z.string(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const patchText = args.patch as string;

      // Parse the patch
      let hunks;
      try {
        hunks = parsePatch(patchText);
      } catch (e) {
        throw new Error(`Patch parse error: ${e instanceof Error ? e.message : e}`);
      }

      if (hunks.length === 0) {
        throw new Error("Empty patch: no file operations found");
      }

      // Resolve all paths and ask permission
      const allPaths = hunks.map((h) =>
        path.relative(
          context.worktree,
          path.resolve(context.directory, h.path),
        ),
      );

      await context.ask({
        permission: "edit",
        patterns: allPaths,
        always: ["*"],
        metadata: {},
      });

      // Process each hunk
      const results: string[] = [];

      for (const hunk of hunks) {
        const filePath = path.resolve(context.directory, hunk.path);

        switch (hunk.type) {
          case "add": {
            await bridge.send("write", {
              file: filePath,
              content: hunk.contents.endsWith("\n") ? hunk.contents : hunk.contents + "\n",
              create_dirs: true,
            });
            results.push(`Created ${hunk.path}`);
            break;
          }

          case "delete": {
            try {
              await fs.promises.unlink(filePath);
              results.push(`Deleted ${hunk.path}`);
            } catch (e) {
              results.push(`Failed to delete ${hunk.path}: ${e instanceof Error ? e.message : e}`);
            }
            break;
          }

          case "update": {
            // Read original, apply chunks, write back
            const original = await fs.promises.readFile(filePath, "utf-8");
            const newContent = applyUpdateChunks(original, filePath, hunk.chunks);

            const targetPath = hunk.move_path
              ? path.resolve(context.directory, hunk.move_path)
              : filePath;

            await bridge.send("write", {
              file: targetPath,
              content: newContent,
              create_dirs: true,
            });

            if (hunk.move_path) {
              await fs.promises.unlink(filePath);
              results.push(`Updated and moved ${hunk.path} → ${hunk.move_path}`);
            } else {
              results.push(`Updated ${hunk.path}`);
            }
            break;
          }
        }
      }

      return results.join("\n");
    },
  };
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

const DELETE_DESCRIPTION =
  "Delete a file with backup (recoverable via aft_safety undo).\n\n" +
  "Parameters:\n" +
  "- file (string, required): Path to file to delete. Relative paths resolved from project root.\n\n" +
  "Returns: { file, deleted, backup_id } on success.\n" +
  "The file content is backed up before deletion — use aft_safety undo to recover if needed.";

function createDeleteTool(ctx: PluginContext): ToolDefinition {
  return {
    description: DELETE_DESCRIPTION,
    args: {
      file: z.string(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const filePath = path.isAbsolute(args.file as string)
        ? (args.file as string)
        : path.resolve(context.directory, args.file as string);

      await context.ask({
        permission: "edit",
        patterns: [filePath],
        always: ["*"],
        metadata: { action: "delete" },
      });

      const result = await bridge.send("delete_file", { file: filePath });
      return JSON.stringify(result);
    },
  };
}

// ---------------------------------------------------------------------------
// Move / Rename
// ---------------------------------------------------------------------------

const MOVE_DESCRIPTION =
  "Move or rename a file with backup (recoverable via aft_safety undo).\n\n" +
  "Parameters:\n" +
  "- file (string, required): Source file path to move.\n" +
  "- destination (string, required): Destination file path.\n\n" +
  "Creates parent directories for destination automatically.\n" +
  "Falls back to copy+delete for cross-filesystem moves.\n" +
  "Returns: { file, destination, moved, backup_id } on success.";

function createMoveTool(ctx: PluginContext): ToolDefinition {
  return {
    description: MOVE_DESCRIPTION,
    args: {
      file: z.string(),
      destination: z.string(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const filePath = path.isAbsolute(args.file as string)
        ? (args.file as string)
        : path.resolve(context.directory, args.file as string);
      const destPath = path.isAbsolute(args.destination as string)
        ? (args.destination as string)
        : path.resolve(context.directory, args.destination as string);

      await context.ask({
        permission: "edit",
        patterns: [filePath, destPath],
        always: ["*"],
        metadata: { action: "move" },
      });

      const result = await bridge.send("move_file", {
        file: filePath,
        destination: destPath,
      });
      return JSON.stringify(result);
    },
  };
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

/**
 * Returns hoisted tools keyed by opencode's built-in names.
 * Overrides: read, write, edit, apply_patch.
 */
export function hoistedTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    read: createReadTool(ctx),
    write: createWriteTool(ctx),
    edit: createEditTool(ctx),
    apply_patch: createApplyPatchTool(ctx),
    aft_delete: createDeleteTool(ctx),
    aft_move: createMoveTool(ctx),
  };
}

/**
 * Returns the same tools with aft_ prefix (for when hoisting is disabled).
 */
export function aftPrefixedTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_read: createReadTool(ctx),
    aft_write: createWriteTool(ctx),
    aft_edit: createEditTool(ctx),
    aft_apply_patch: createApplyPatchTool(ctx),
    aft_delete: createDeleteTool(ctx),
    aft_move: createMoveTool(ctx),
  };
}
