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

import * as fs from "node:fs";
import * as path from "node:path";
import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import { storeToolMetadata } from "../metadata-store.js";
import { applyUpdateChunks, parsePatch } from "../patch-parser.js";
import type { PluginContext } from "../types.js";

/** Extract callID from plugin context (exists on object but not in TS type). */
function getCallID(ctx: unknown): string | undefined {
  const c = ctx as { callID?: string; callId?: string; call_id?: string };
  return c.callID ?? c.callId ?? c.call_id;
}

/** Get relative path matching opencode's format — the desktop UI parses it to extract filename + dir. */
function relativeToWorktree(fp: string, worktree: string): string {
  return path.relative(worktree, fp);
}

/** Build a simple unified diff string from before/after content. */
function buildUnifiedDiff(fp: string, before: string, after: string): string {
  const beforeLines = before.split("\n");
  const afterLines = after.split("\n");
  let diff = `Index: ${fp}\n===================================================================\n--- ${fp}\n+++ ${fp}\n`;
  let firstChange = -1;
  let lastChange = -1;
  const maxLen = Math.max(beforeLines.length, afterLines.length);
  for (let i = 0; i < maxLen; i++) {
    if ((beforeLines[i] ?? "") !== (afterLines[i] ?? "")) {
      if (firstChange === -1) firstChange = i;
      lastChange = i;
    }
  }
  if (firstChange === -1) return diff;
  const ctxStart = Math.max(0, firstChange - 2);
  const ctxEnd = Math.min(maxLen - 1, lastChange + 2);
  diff += `@@ -${ctxStart + 1},${Math.min(beforeLines.length, ctxEnd + 1) - ctxStart} +${ctxStart + 1},${Math.min(afterLines.length, ctxEnd + 1) - ctxStart} @@\n`;
  for (let i = ctxStart; i <= ctxEnd; i++) {
    const bl = i < beforeLines.length ? beforeLines[i] : undefined;
    const al = i < afterLines.length ? afterLines[i] : undefined;
    if (bl === al) {
      diff += ` ${bl}\n`;
    } else {
      if (bl !== undefined) diff += `-${bl}\n`;
      if (al !== undefined) diff += `+${al}\n`;
    }
  }
  return diff;
}

const z = tool.schema;

// ---------------------------------------------------------------------------
// Descriptions — verbose because .describe() on Zod args does NOT reach the agent.
// The description string is the ONLY documentation the LLM sees.
// ---------------------------------------------------------------------------

const READ_DESCRIPTION = `Read file contents or list directory entries.

Parameters:
- filePath (string, required): Path to file or directory (absolute or relative to project root)
- start_line (number): 1-based line to start reading from
- end_line (number): 1-based line to stop reading at (inclusive)
- offset (number): Line number to start reading from (use with limit)
- limit (number): Max lines to return (default: 2000)

Use either start_line/end_line OR offset/limit to read a section of a file.

Behavior:
- Returns line-numbered content (e.g., "1: const x = 1")
- Lines longer than 2000 characters are truncated
- Output capped at 50KB
- Binary files are auto-detected and return a size-only message
- Directories return sorted entries with trailing / for subdirectories

Examples:
  Read full file: { "filePath": "src/app.ts" }
  Read lines 50-100: { "filePath": "src/app.ts", "start_line": 50, "end_line": 100 }
  Read 30 lines from line 200: { "filePath": "src/app.ts", "offset": 200, "limit": 30 }
  List directory: { "filePath": "src/" }`;

/**
 * Creates the simple read tool. Registers as "read" when hoisted, "aft_read" when not.
 */
export function createReadTool(ctx: PluginContext): ToolDefinition {
  return {
    description: READ_DESCRIPTION,
    args: {
      filePath: z.string(),
      start_line: z.number().optional(),
      end_line: z.number().optional(),
      limit: z.number().optional(),
      offset: z.number().optional(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const file = (args.filePath ?? args.file) as string;

      // Resolve relative paths
      const filePath = path.isAbsolute(file) ? file : path.resolve(context.directory, file);

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
        ".png": "image/png",
        ".jpg": "image/jpeg",
        ".jpeg": "image/jpeg",
        ".gif": "image/gif",
        ".webp": "image/webp",
        ".bmp": "image/bmp",
        ".ico": "image/x-icon",
        ".tiff": "image/tiff",
        ".tif": "image/tiff",
        ".avif": "image/avif",
        ".heic": "image/heic",
        ".heif": "image/heif",
        ".pdf": "application/pdf",
      };
      const mime = mimeMap[ext];
      if (mime) {
        const isImage = mime.startsWith("image/");
        const label = isImage ? "Image" : "PDF";
        let fileSize = 0;
        try {
          const stat = await import("node:fs/promises").then((fs) => fs.stat(filePath));
          fileSize = stat.size;
        } catch {
          /* ignore */
        }
        const sizeStr =
          fileSize > 1024 * 1024
            ? `${(fileSize / (1024 * 1024)).toFixed(1)}MB`
            : fileSize > 1024
              ? `${(fileSize / 1024).toFixed(0)}KB`
              : `${fileSize} bytes`;
        const msg = `${label} read successfully`;
        const imgCallID = getCallID(context);
        if (imgCallID) {
          storeToolMetadata(context.sessionID, imgCallID, {
            title: path.relative(context.worktree, filePath),
            metadata: {
              preview: msg,
              filepath: filePath,
              isImage,
              isPdf: mime === "application/pdf",
            },
          });
        }
        return `${msg} (${ext.slice(1).toUpperCase()}, ${sizeStr}). File: ${filePath}`;
      }

      const _relPath = path.relative(context.worktree, filePath);

      // Normalize offset/limit to start_line/end_line (backward compat with opencode's read)
      let startLine = args.start_line;
      let endLine = args.end_line;
      if (startLine === undefined && args.offset !== undefined) {
        startLine = args.offset;
        if (args.limit !== undefined) {
          endLine = Number(args.offset) + Number(args.limit) - 1;
        }
      }

      // Always use Rust read command — simple file reading only
      const params: Record<string, unknown> = { file: filePath };
      if (startLine !== undefined) params.start_line = startLine;
      if (endLine !== undefined) params.end_line = endLine;
      if (args.limit !== undefined) params.limit = args.limit;

      const data = await bridge.send("read", params);

      const readCallID = getCallID(context);

      // Directory response
      if (data.entries) {
        if (readCallID) {
          const dp = relativeToWorktree(filePath, context.worktree) || file;
          storeToolMetadata(context.sessionID, readCallID, { title: dp, metadata: { title: dp } });
        }
        return (data.entries as string[]).join("\n");
      }

      // Binary response
      if (data.binary) {
        if (readCallID) {
          const dp = relativeToWorktree(filePath, context.worktree) || file;
          storeToolMetadata(context.sessionID, readCallID, { title: dp, metadata: { title: dp } });
        }
        return data.message as string;
      }

      // File content — already line-numbered from Rust
      if (readCallID) {
        const dp = relativeToWorktree(filePath, context.worktree) || file;
        storeToolMetadata(context.sessionID, readCallID, { title: dp, metadata: { title: dp } });
      }
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
- \`filePath\` (string, required): Path to the file to write (absolute or relative to project root)
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
      filePath: z.string(),
      content: z.string(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const file = (args.filePath ?? args.file) as string;
      const content = args.content as string;

      const filePath = path.isAbsolute(file) ? file : path.resolve(context.directory, file);

      const relPath = path.relative(context.worktree, filePath);

      // Permission check
      await context.ask({
        permission: "edit",
        patterns: [relPath],
        always: ["*"],
        metadata: { filepath: filePath },
      });

      const data = await bridge.send("write", {
        file: filePath,
        content,
        create_dirs: true,
        diagnostics: true,
        include_diff: true,
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

      // Store metadata for tool.execute.after hook (fromPlugin overwrites context.metadata)
      const diff = data.diff as
        | { before?: string; after?: string; additions?: number; deletions?: number }
        | undefined;
      const callID = getCallID(context);
      if (callID) {
        const dp = relativeToWorktree(filePath, context.worktree);
        const beforeContent = diff?.before ?? "";
        const afterContent = diff?.after ?? content;
        storeToolMetadata(context.sessionID, callID, {
          title: dp,
          metadata: {
            diff: buildUnifiedDiff(filePath, beforeContent, afterContent),
            filediff: {
              file: filePath,
              before: beforeContent,
              after: afterContent,
              additions: diff?.additions ?? 0,
              deletions: diff?.deletions ?? 0,
            },
            diagnostics: {},
          },
        });
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

1. **Find and replace** — pass \`filePath\` + \`oldString\` + \`newString\`
   Finds the exact text in \`oldString\` and replaces it with \`newString\`.
   Supports fuzzy matching (handles whitespace differences automatically).
   If multiple matches exist, specify which one with \`occurrence\` or use \`replaceAll: true\`.
   Example: \`{ "filePath": "src/app.ts", "oldString": "const x = 1", "newString": "const x = 2" }\`

2. **Replace all occurrences** — add \`replaceAll: true\`
   Replaces every occurrence of \`oldString\` in the file.
   Example: \`{ "filePath": "src/app.ts", "oldString": "oldName", "newString": "newName", "replaceAll": true }\`

3. **Select specific occurrence** — add \`occurrence: N\` (0-indexed)
   When multiple matches exist, select the Nth one (0 = first, 1 = second, etc.).
   Example: \`{ "filePath": "src/app.ts", "oldString": "TODO", "newString": "DONE", "occurrence": 0 }\`

4. **Symbol replace** — pass \`filePath\` + \`symbol\` + \`content\`
   Replaces an entire named symbol (function, class, type) with new content.
   Includes decorators, attributes, and doc comments in the replacement range.
   Example: \`{ "filePath": "src/app.ts", "symbol": "handleRequest", "content": "function handleRequest() { ... }" }\`

5. **Batch edits** — pass \`filePath\` + \`edits\` array
   Multiple edits in one file atomically. Each edit is either:
   - \`{ "oldString": "old", "newString": "new" }\` — find/replace
   - \`{ "line_start": 5, "line_end": 7, "content": "new lines" }\` — replace line range (1-based, inclusive)
   Set content to empty string to delete lines.

6. **Multi-file transaction** — pass \`operations\` array
   Atomic edits across multiple files with rollback on failure.

**Parameters:**
- \`filePath\` (string): Path to file, or glob pattern for multi-file operations
- \`oldString\` (string): Text to find (exact match, with fuzzy fallback)
- \`newString\` (string): Text to replace with
- \`replaceAll\` (boolean): Replace all occurrences
- \`occurrence\` (number): 0-indexed occurrence to replace when multiple matches exist
- \`symbol\` (string): Named symbol to replace (function, class, type)
- \`content\` (string): New content for symbol replace or file write
- \`edits\` (array): Batch edits — array of { oldString, newString } or { line_start, line_end, content }
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
      filePath: z.string().optional(),
      oldString: z.string().optional(),
      newString: z.string().optional(),
      replaceAll: z.boolean().optional(),
      occurrence: z.number().optional(),
      symbol: z.string().optional(),
      content: z.string().optional(),
      edits: z.array(z.record(z.string(), z.unknown())).optional(),
      operations: z.array(z.record(z.string(), z.unknown())).optional(),
      dry_run: z.boolean().optional(),
      diagnostics: z.boolean().optional(),
      createFile: z.boolean().optional(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);

      // Transaction mode — multi-file
      if (Array.isArray(args.operations)) {
        const ops = args.operations as Array<Record<string, unknown>>;
        const files = ops.map((op) => op.file as string).filter(Boolean);

        await context.ask({
          permission: "edit",
          patterns: files.map((f) =>
            path.relative(context.worktree, path.resolve(context.directory, f)),
          ),
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

      const file = (args.filePath ?? args.file) as string;
      if (!file) throw new Error("'file' parameter is required");

      const filePath = path.isAbsolute(file) ? file : path.resolve(context.directory, file);

      const relPath = path.relative(context.worktree, filePath);

      await context.ask({
        permission: "edit",
        patterns: [relPath],
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
      } else if (typeof args.symbol === "string" && typeof args.oldString !== "string" && args.content !== undefined) {
        // Symbol replace — only when content is provided and oldString is NOT present
        // (agents often pass symbol as "what to search for", not "replace whole symbol")
        command = "edit_symbol";
        params.symbol = args.symbol;
        params.operation = "replace";
        params.content = args.content;
      } else if (typeof args.oldString === "string") {
        // Find/replace mode — default newString to "" (deletion) if not provided
        command = "edit_match";
        params.match = args.oldString;
        params.replacement = args.newString ?? "";
        if (args.replaceAll !== undefined) params.replace_all = args.replaceAll;
        if (args.occurrence !== undefined) params.occurrence = args.occurrence;
      } else if (typeof args.content === "string" || args.createFile) {
        // Write mode
        command = "write";
        params.content = args.content ?? "";
        params.create_dirs = true;
      } else {
        throw new Error(
          "Provide 'oldString' + 'newString', 'symbol' + 'content', 'edits' array, or 'content' for write",
        );
      }

      if (args.dry_run) params.dry_run = true;
      if (args.diagnostics) params.diagnostics = true;
      // Request diff from Rust for UI metadata (avoids extra file reads in TS)
      if (!args.dry_run) params.include_diff = true;

      const data = await bridge.send(command, params);

      // Store metadata for tool.execute.after hook (fromPlugin overwrites context.metadata)
      if (!args.dry_run && data.success && data.diff) {
        const diff = data.diff as {
          before?: string;
          after?: string;
          additions?: number;
          deletions?: number;
        };
        const callID = getCallID(context);
        if (callID) {
          const dp = relativeToWorktree(filePath, context.worktree);
          const beforeContent = diff.before ?? "";
          const afterContent = diff.after ?? "";
          storeToolMetadata(context.sessionID, callID, {
            title: dp,
            metadata: {
              diff: buildUnifiedDiff(filePath, beforeContent, afterContent),
              filediff: {
                file: filePath,
                before: beforeContent,
                after: afterContent,
                additions: diff.additions ?? 0,
                deletions: diff.deletions ?? 0,
              },
              diagnostics: {},
            },
          });
        }
      }

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
      patch: z.string().optional(),
      patchText: z.string().optional(),  // backward compat with opencode's apply_patch
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const patchText = (args.patch ?? args.patchText) as string;
      if (!patchText) throw new Error("'patch' or 'patchText' is required");

      // Parse the patch
      let hunks: import("../patch-parser.js").Hunk[];
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
        path.relative(context.worktree, path.resolve(context.directory, h.path)),
      );

      await context.ask({
        permission: "edit",
        patterns: allPaths,
        always: ["*"],
        metadata: {},
      });

      // Process each hunk, track diffs for metadata
      const results: string[] = [];
      let totalAdditions = 0;
      let totalDeletions = 0;
      let combinedBefore = "";
      let combinedAfter = "";

      for (const hunk of hunks) {
        const filePath = path.resolve(context.directory, hunk.path);

        switch (hunk.type) {
          case "add": {
            await bridge.send("write", {
              file: filePath,
              content: hunk.contents.endsWith("\n") ? hunk.contents : `${hunk.contents}\n`,
              create_dirs: true,
            });
            const lines = hunk.contents.split("\n").length;
            totalAdditions += lines;
            combinedAfter += hunk.contents;
            results.push(`Created ${hunk.path}`);
            break;
          }

          case "delete": {
            try {
              const before = await fs.promises.readFile(filePath, "utf-8").catch(() => "");
              await fs.promises.unlink(filePath);
              const lines = before.split("\n").length;
              totalDeletions += lines;
              combinedBefore += before;
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

            // Track diff
            combinedBefore += original;
            combinedAfter += newContent;
            const origLines = original.split("\n");
            const newLines = newContent.split("\n");
            // Count changed lines using simple comparison
            let adds = 0;
            let dels = 0;
            const maxLen = Math.max(origLines.length, newLines.length);
            for (let i = 0; i < maxLen; i++) {
              if ((origLines[i] ?? "") !== (newLines[i] ?? "")) {
                if (i < origLines.length) dels++;
                if (i < newLines.length) adds++;
              }
            }
            totalAdditions += adds;
            totalDeletions += dels;

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

      // Store metadata for tool.execute.after hook (match opencode built-in format)
      const callID = getCallID(context);
      if (callID) {
        // Build per-file metadata matching opencode's files array
        const files = hunks.map((h) => {
          const relPath = path.relative(context.worktree, path.resolve(context.directory, h.path));
          return {
            filePath: path.resolve(context.directory, h.path),
            relativePath: relPath,
            type: h.type,
          };
        });

        // Build title matching built-in: "Success. Updated the following files:\nM path/to/file.ts"
        const fileList = files.map((f) => {
          const prefix = f.type === "add" ? "A" : f.type === "delete" ? "D" : "M";
          return `${prefix} ${f.relativePath}`;
        }).join("\n");
        const title = `Success. Updated the following files:\n${fileList}`;

        storeToolMetadata(context.sessionID, callID, {
          title,
          metadata: {
            diff: buildUnifiedDiff(
              files.length === 1 ? files[0].filePath : "patch",
              combinedBefore,
              combinedAfter,
            ),
            files,
          },
        });
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
