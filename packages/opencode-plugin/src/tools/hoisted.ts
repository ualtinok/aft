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
import { callBridge } from "./_shared.js";

/** Extract callID from plugin context (exists on object but not in TS type). */
function getCallID(ctx: unknown): string | undefined {
  const c = ctx as { callID?: string; callId?: string; call_id?: string };
  return c.callID ?? c.callId ?? c.call_id;
}

/** Get relative path matching opencode's format — the desktop UI parses it to extract filename + dir. */
function relativeToWorktree(fp: string, worktree: string): string {
  return path.relative(worktree, fp);
}

/** Test-only export. Production code uses buildUnifiedDiff directly. */
export const _buildUnifiedDiffForTest = (fp: string, before: string, after: string): string =>
  buildUnifiedDiff(fp, before, after);

/**
 * Build a unified diff string from before/after content using a proper
 * LCS-based diff algorithm with grouped hunks and 3 lines of context.
 *
 * The previous implementation compared lines by index, so any insertion
 * or deletion that shifted line numbers caused every subsequent line to
 * compare unequal — emitting the entire rest of the file as "changed"
 * (issue #22, regression introduced in v0.15.3 when apply_patch started
 * sending diffs).
 *
 * Output matches GNU diff -u style: --- /+++ headers, @@ hunk markers,
 * one hunk per change cluster (consecutive changes within 6 lines of
 * each other are merged into a single hunk).
 */
function buildUnifiedDiff(fp: string, before: string, after: string): string {
  // Skip diff for very large files to avoid blocking the event loop
  const SIZE_CAP = 100 * 1024; // 100KB
  if (before.length > SIZE_CAP || after.length > SIZE_CAP) {
    return `Index: ${fp}\n(diff skipped: file exceeds ${SIZE_CAP / 1024}KB)\n`;
  }

  const beforeLines = before.split("\n");
  const afterLines = after.split("\n");
  const ops = diffLines(beforeLines, afterLines);

  // No changes → empty diff (caller decides whether to render the header).
  if (ops.every((op) => op.tag === "eq")) {
    return `Index: ${fp}\n===================================================================\n--- ${fp}\n+++ ${fp}\n`;
  }

  const CONTEXT = 3;
  const HUNK_GAP = CONTEXT * 2; // merge hunks closer than this
  const hunks = groupIntoHunks(ops, CONTEXT, HUNK_GAP, beforeLines.length, afterLines.length);

  let diff = `Index: ${fp}\n===================================================================\n--- ${fp}\n+++ ${fp}\n`;
  for (const hunk of hunks) {
    diff += `@@ -${hunk.beforeStart},${hunk.beforeCount} +${hunk.afterStart},${hunk.afterCount} @@\n`;
    for (const line of hunk.lines) {
      diff += `${line}\n`;
    }
  }
  return diff;
}

/**
 * Count non-empty lines in a string. Used for unambiguous addition/deletion
 * counts when one side of a diff is empty (apply_patch's add/delete hunks).
 *
 * `split("\n")` on a string with a trailing newline produces a trailing
 * empty element which we drop, so the count matches "actual content lines"
 * rather than "split slots". For empty input the count is 0.
 */
function lineCount(content: string): number {
  if (content.length === 0) return 0;
  const parts = content.split("\n");
  // Drop the trailing empty element produced by a terminating "\n".
  if (parts[parts.length - 1] === "") parts.pop();
  return parts.length;
}

/**
 * Count additions and deletions between two file contents using the same
 * LCS path that powers buildUnifiedDiff. Used for apply_patch's *move*
 * case where the Rust write diff would compare against an empty target
 * (overcounting additions). For non-move updates we use the Rust counts
 * directly.
 */
function countDiffLines(before: string, after: string): { additions: number; deletions: number } {
  const beforeLines = before.split("\n");
  const afterLines = after.split("\n");
  const ops = diffLines(beforeLines, afterLines);
  let additions = 0;
  let deletions = 0;
  for (const op of ops) {
    if (op.tag === "ins") additions++;
    else if (op.tag === "del") deletions++;
  }
  return { additions, deletions };
}

type DiffOp =
  | { tag: "eq"; beforeIdx: number; afterIdx: number; line: string }
  | { tag: "del"; beforeIdx: number; line: string }
  | { tag: "ins"; afterIdx: number; line: string };

/**
 * LCS-based line diff. Builds a length table then walks back to produce ops.
 * O(n*m) time and space — fine for the 100KB SIZE_CAP guard above.
 */
function diffLines(a: readonly string[], b: readonly string[]): DiffOp[] {
  const n = a.length;
  const m = b.length;

  // dp[i][j] = LCS length of a[0..i] and b[0..j]
  // Use a flat Uint32Array for memory efficiency on large files.
  const dp = new Uint32Array((n + 1) * (m + 1));
  const w = m + 1;
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      if (a[i - 1] === b[j - 1]) {
        dp[i * w + j] = dp[(i - 1) * w + (j - 1)] + 1;
      } else {
        const up = dp[(i - 1) * w + j];
        const left = dp[i * w + (j - 1)];
        dp[i * w + j] = up >= left ? up : left;
      }
    }
  }

  // Walk back to produce ops in reverse, then reverse at the end.
  const ops: DiffOp[] = [];
  let i = n;
  let j = m;
  while (i > 0 && j > 0) {
    if (a[i - 1] === b[j - 1]) {
      ops.push({ tag: "eq", beforeIdx: i - 1, afterIdx: j - 1, line: a[i - 1] });
      i--;
      j--;
    } else if (dp[(i - 1) * w + j] >= dp[i * w + (j - 1)]) {
      ops.push({ tag: "del", beforeIdx: i - 1, line: a[i - 1] });
      i--;
    } else {
      ops.push({ tag: "ins", afterIdx: j - 1, line: b[j - 1] });
      j--;
    }
  }
  while (i > 0) {
    ops.push({ tag: "del", beforeIdx: i - 1, line: a[i - 1] });
    i--;
  }
  while (j > 0) {
    ops.push({ tag: "ins", afterIdx: j - 1, line: b[j - 1] });
    j--;
  }
  ops.reverse();
  return ops;
}

interface Hunk {
  beforeStart: number; // 1-based
  beforeCount: number;
  afterStart: number; // 1-based
  afterCount: number;
  lines: string[]; // each prefixed with " ", "+", or "-"
}

/**
 * Group ops into hunks. Consecutive change ops are clustered with `context`
 * lines on each side; clusters closer than `gap` are merged into one hunk.
 */
function groupIntoHunks(
  ops: DiffOp[],
  context: number,
  gap: number,
  beforeLen: number,
  afterLen: number,
): Hunk[] {
  // Find indices of change ops (ins or del).
  const changeIdx: number[] = [];
  for (let k = 0; k < ops.length; k++) {
    if (ops[k].tag !== "eq") changeIdx.push(k);
  }
  if (changeIdx.length === 0) return [];

  // Build hunk ranges in op-index space, then merge nearby ones.
  const ranges: Array<[number, number]> = [];
  for (const idx of changeIdx) {
    const start = Math.max(0, idx - context);
    const end = Math.min(ops.length - 1, idx + context);
    if (ranges.length > 0 && start <= ranges[ranges.length - 1][1] + gap) {
      ranges[ranges.length - 1][1] = Math.max(ranges[ranges.length - 1][1], end);
    } else {
      ranges.push([start, end]);
    }
  }

  // Materialize each range as a hunk. Track 1-based line numbers from the
  // first op's recorded indices.
  const hunks: Hunk[] = [];
  for (const [start, end] of ranges) {
    let beforeStart = -1;
    let afterStart = -1;
    let beforeCount = 0;
    let afterCount = 0;
    const lines: string[] = [];
    for (let k = start; k <= end; k++) {
      const op = ops[k];
      if (op.tag === "eq") {
        if (beforeStart === -1) beforeStart = op.beforeIdx + 1;
        if (afterStart === -1) afterStart = op.afterIdx + 1;
        beforeCount++;
        afterCount++;
        lines.push(` ${op.line}`);
      } else if (op.tag === "del") {
        if (beforeStart === -1) beforeStart = op.beforeIdx + 1;
        if (afterStart === -1) {
          // Pure-deletion hunk at start: position after-cursor is one past
          // the last preceding equal op. Walk forward to find the next
          // ins/eq to anchor afterStart, otherwise clamp to end.
          afterStart = inferAfterStart(ops, k, afterLen);
        }
        beforeCount++;
        lines.push(`-${op.line}`);
      } else {
        if (afterStart === -1) afterStart = op.afterIdx + 1;
        if (beforeStart === -1) {
          beforeStart = inferBeforeStart(ops, k, beforeLen);
        }
        afterCount++;
        lines.push(`+${op.line}`);
      }
    }
    // Empty file edge case: GNU diff uses 0 for line numbers when count is 0.
    if (beforeCount === 0) beforeStart = 0;
    if (afterCount === 0) afterStart = 0;
    hunks.push({ beforeStart, beforeCount, afterStart, afterCount, lines });
  }
  return hunks;
}

/** Find what afterStart should be when a hunk begins with deletions. */
function inferAfterStart(ops: DiffOp[], from: number, afterLen: number): number {
  // Look forward for any op carrying an afterIdx.
  for (let k = from; k < ops.length; k++) {
    const op = ops[k];
    if (op.tag === "eq") return op.afterIdx + 1;
    if (op.tag === "ins") return op.afterIdx + 1;
  }
  // No future after-line — point past the last line.
  return afterLen;
}

/** Find what beforeStart should be when a hunk begins with insertions. */
function inferBeforeStart(ops: DiffOp[], from: number, beforeLen: number): number {
  for (let k = from; k < ops.length; k++) {
    const op = ops[k];
    if (op.tag === "eq") return op.beforeIdx + 1;
    if (op.tag === "del") return op.beforeIdx + 1;
  }
  return beforeLen;
}

const z = tool.schema;

// ---------------------------------------------------------------------------
// Tool descriptions focus on behavior, modes, and return values.
// Parameter docs live in Zod .describe() and reach the LLM via JSON Schema.
// ---------------------------------------------------------------------------

const READ_DESCRIPTION = `Read file contents or list directory entries.

Use either startLine/endLine OR offset/limit to read a section of a file.

Behavior:
- Returns line-numbered content (e.g., "1: const x = 1")
- Lines longer than 2000 characters are truncated
- Output capped at 50KB
- Binary files are auto-detected and return a size-only message
- Image files (.png, .jpg, .gif, .webp, etc.) and PDFs return a metadata string (format, size, path) — no file content is returned
- Directories return sorted entries with trailing / for subdirectories

Examples:
  Read full file: { "filePath": "src/app.ts" }
  Read lines 50-100: { "filePath": "src/app.ts", "startLine": 50, "endLine": 100 }
  Read 30 lines from line 200: { "filePath": "src/app.ts", "offset": 200, "limit": 30 }
  List directory: { "filePath": "src/" }

Returns: Line-numbered file content string. For directories: newline-joined sorted entries. For binary files: size/message string.`;

/**
 * Creates the simple read tool. Registers as "read" when hoisted, "aft_read" when not.
 */
export function createReadTool(ctx: PluginContext): ToolDefinition {
  return {
    description: READ_DESCRIPTION,
    args: {
      filePath: z
        .string()
        .describe("Path to file or directory (absolute or relative to project root)"),
      startLine: z.number().optional().describe("1-based line to start reading from"),
      endLine: z.number().optional().describe("1-based line to stop reading at (inclusive)"),
      limit: z.number().optional().describe("Max lines to return (default: 2000)"),
      offset: z
        .number()
        .optional()
        .describe(
          "1-based line number to start reading from (use with limit). Ignored if startLine is provided",
        ),
    },
    execute: async (args, context): Promise<string> => {
      const file = args.filePath as string;

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

      // Normalize offset/limit to startLine/endLine (backward compat with opencode's read)
      let startLine = args.startLine;
      let endLine = args.endLine;
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
      // Only send limit if we did NOT convert offset to startLine/endLine
      if (args.limit !== undefined && args.offset === undefined) params.limit = args.limit;

      const data = await callBridge(ctx, context, "read", params);

      // Error response (e.g. file not found)
      if (data.success === false) {
        throw new Error((data.message as string) || "read failed");
      }

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
        output += `\n(Showing lines ${data.start_line}-${data.end_line} of ${data.total_lines}. Use startLine/endLine to read other sections.)`;
      }

      return output;
    },
  };
}

// ---------------------------------------------------------------------------
// WRITE tool
// ---------------------------------------------------------------------------

function getWriteDescription(editToolName: string): string {
  return `Write content to a file, creating it (and parent directories) if needed.

Automatically creates parent directories. Backs up existing files before overwriting.
If the project has a formatter configured (biome, prettier, rustfmt, etc.), the file
is auto-formatted after writing. Returns inline LSP diagnostics when available.

**Behavior:**
- Creates parent directories automatically (no need to mkdir first)
- Existing files are backed up before overwriting (recoverable via aft_safety undo)
- Auto-formats using project formatter if configured (biome.json, .prettierrc, etc.)
- Returns LSP error-level diagnostics inline if type errors are introduced
- Use this for creating new files or completely replacing file contents
- For partial edits (find/replace), use the \`${editToolName}\` tool instead

Returns: Status message string (for example: "Created new file. Auto-formatted.") with optional inline LSP error lines.`;
}

function createWriteTool(ctx: PluginContext, editToolName = "edit"): ToolDefinition {
  return {
    description: getWriteDescription(editToolName),
    args: {
      filePath: z
        .string()
        .describe("Path to the file to write (absolute or relative to project root)"),
      content: z.string().describe("The full content to write to the file"),
    },
    execute: async (args, context): Promise<string> => {
      const file = args.filePath as string;
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

      const data = await callBridge(ctx, context, "write", {
        file: filePath,
        content,
        create_dirs: true,
        diagnostics: true,
        include_diff: true,
      });

      // Error response (e.g. path validation failure)
      if (data.success === false) {
        throw new Error((data.message as string) || "write failed");
      }

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

function getEditDescription(writeToolName: string): string {
  return `Edit a file by finding and replacing text, or by targeting named symbols. To write or overwrite a whole file, use the \`${writeToolName}\` tool — \`edit\` requires an explicit edit mode and will not silently overwrite a file from \`content\` alone.

**Modes** (determined by which parameters you provide):

Mode priority: operations > edits > symbol (without oldString) > oldString (find/replace). If none match, the call is rejected — there is no implicit "write" fallback.

1. **Multi-file transaction** — pass \`operations\` array
   Edits across multiple files with checkpoint-based rollback on failure.
   Each operation: \`{ "file": "path", "command": "edit_match" | "write", ... }\`.
   For \`edit_match\`: include \`match\`, \`replacement\`. For \`write\`: include \`content\`.
   Example: \`{ "operations": [{ "file": "a.ts", "command": "edit_match", "match": "old", "replacement": "new" }, { "file": "b.ts", "command": "write", "content": "..." }] }\`

2. **Batch edits** — pass \`filePath\` + \`edits\` array
   Multiple edits in one file atomically. Each edit is either:
   - \`{ "oldString": "old", "newString": "new" }\` — find/replace
   - \`{ "startLine": 5, "endLine": 7, "content": "new lines" }\` — replace line range (1-based, both inclusive)
   Set content to empty string to delete lines.

3. **Symbol replace** — pass \`filePath\` + \`symbol\` + \`content\`
   Replaces an entire named symbol (function, class, type) with new content.
   Includes decorators, attributes, and doc comments in the replacement range.
   **Important:** You must NOT provide \`oldString\` when using symbol mode — if present, the tool silently falls back to find/replace mode.
   Example: \`{ "filePath": "src/app.ts", "symbol": "handleRequest", "content": "function handleRequest() { ... }" }\`

4. **Find and replace** — pass \`filePath\` + \`oldString\` + \`newString\`
   Finds the exact text in \`oldString\` and replaces it with \`newString\`.
   Supports fuzzy matching (handles whitespace differences automatically).
   If multiple matches exist, specify which one with \`occurrence\` or use \`replaceAll: true\`.
   Example: \`{ "filePath": "src/app.ts", "oldString": "const x = 1", "newString": "const x = 2" }\`

5. **Replace all occurrences** — add \`replaceAll: true\`
   Replaces every occurrence of \`oldString\` in the file.
   Example: \`{ "filePath": "src/app.ts", "oldString": "oldName", "newString": "newName", "replaceAll": true }\`

6. **Select specific occurrence** — add \`occurrence: N\` (0-indexed)
   When multiple matches exist, select the Nth one (0 = first, 1 = second, etc.).
   Example: \`{ "filePath": "src/app.ts", "oldString": "TODO", "newString": "DONE", "occurrence": 0 }\`

Note: Modes 5 and 6 are options on mode 4 (find/replace) — they require \`oldString\`.

**Behavior:**
- Backs up files before editing (recoverable via aft_safety undo)
- Auto-formats using project formatter if configured
- Tree-sitter syntax validation on all edits
- Symbol replace includes decorators, attributes, and doc comments in range
- LSP error-level diagnostics are returned automatically after non-dry-run edits

Returns: JSON string for the selected edit mode. Dry runs return diff data; non-dry-run edits may append inline LSP error lines.

Common response fields: success (boolean), diff (object with before/after), backup_id (string), syntax_valid (boolean). Exact fields vary by mode.`;
  // Note: The Returns section intentionally stays high-level because per-mode JSON shapes
  // vary by Rust command and documenting each would bloat the description for minimal gain.
  // Agents can parse the JSON response generically — key fields include 'success' and 'diff'.
}

function createEditTool(ctx: PluginContext, writeToolName = "write"): ToolDefinition {
  return {
    description: getEditDescription(writeToolName),
    args: {
      filePath: z
        .string()
        .optional()
        .describe(
          "Path to the file to edit (absolute or relative to project root). Required for all modes except 'operations' multi-file transactions",
        ),
      oldString: z.string().optional().describe("Text to find (exact match, with fuzzy fallback)"),
      newString: z
        .string()
        .optional()
        .describe("Text to replace with (omit or set to empty string to delete the matched text)"),
      replaceAll: z.boolean().optional().describe("Replace all occurrences"),
      occurrence: z
        .number()
        .optional()
        .describe("0-indexed occurrence to replace when multiple matches exist"),
      symbol: z.string().optional().describe("Named symbol to replace (function, class, type)"),
      content: z.string().optional().describe("New content for symbol replace or file write"),
      edits: z
        .array(z.record(z.string(), z.unknown()))
        .optional()
        .describe(
          "Batch edits — array of { oldString: string, newString: string } or { startLine: number (1-based), endLine: number (1-based, inclusive), content: string }",
        ),
      operations: z
        .array(z.record(z.string(), z.unknown()))
        .optional()
        .describe(
          "Transaction — array of { file: string, command: 'edit_match' | 'write', match?: string, replacement?: string, content?: string } for multi-file edits with rollback. Note: uses 'file'/'match'/'replacement' (not filePath/oldString/newString)",
        ),
      dryRun: z
        .boolean()
        .optional()
        .describe("Preview changes without applying (returns diff, default: false)"),
    },
    execute: async (args, context): Promise<string> => {
      // Footgun guard: top-level startLine/endLine are not valid params on
      // edit. They only exist nested inside `edits[]` for batch line-range
      // mode. Without this guard, Zod silently strips the unknown keys and
      // the call falls through mode resolution to the content-only-write
      // branch, overwriting the entire file. Reject with a helpful pointer.
      const argsRecord = args as Record<string, unknown>;
      if (argsRecord.startLine !== undefined || argsRecord.endLine !== undefined) {
        throw new Error(
          "edit: 'startLine'/'endLine' are not top-level parameters. " +
            "For line-range edits, nest them inside the `edits` array: " +
            '`edits: [{ startLine: N, endLine: M, content: "..." }]`. ' +
            "For find/replace, use `oldString`/`newString` instead.",
        );
      }

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

        const params: Record<string, unknown> = { operations: resolvedOps };
        params.dry_run = args.dryRun === true;
        const data = await callBridge(ctx, context, "transaction", params);
        return JSON.stringify(data);
      }

      const file = args.filePath as string;
      if (!file) throw new Error("'filePath' parameter is required");

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
        // Batch mode — translate camelCase to snake_case for Rust
        command = "batch";
        params.edits = (args.edits as Array<Record<string, unknown>>).map((edit) => {
          const translated: Record<string, unknown> = {};
          for (const [key, value] of Object.entries(edit)) {
            if (key === "oldString") translated.match = value;
            else if (key === "newString") translated.replacement = value;
            else if (key === "startLine") translated.line_start = value;
            else if (key === "endLine") translated.line_end = value;
            else translated[key] = value;
          }
          return translated;
        });
      } else if (
        typeof args.symbol === "string" &&
        typeof args.oldString !== "string" &&
        args.content !== undefined
      ) {
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
      } else {
        // No mode-selecting parameter matched. We deliberately do NOT fall
        // through to a content-only "write" mode here, even when `content` is
        // present: that fallback was the disaster path (a typo or misnamed
        // param like top-level startLine could silently overwrite the whole
        // file). For full-file writes, use the dedicated `${writeToolName}`
        // tool, which is unambiguous about its destructive intent.
        const hint =
          typeof args.content === "string"
            ? ` To write the whole file, use the '${writeToolName}' tool. To edit existing content, provide 'oldString' (and optionally 'newString'), 'symbol' + 'content', or an 'edits' array.`
            : " Provide 'oldString' (+ optional 'newString'), 'symbol' + 'content', 'edits' array, or 'operations' array.";
        throw new Error(`edit: no edit mode resolved from arguments.${hint}`);
      }

      if (args.dryRun) params.dry_run = true;
      if (!args.dryRun) params.diagnostics = true;
      // Request diff from Rust for UI metadata (avoids extra file reads in TS)
      if (!args.dryRun) params.include_diff = true;

      const data = await callBridge(ctx, context, command, params);

      // Store metadata for tool.execute.after hook (fromPlugin overwrites context.metadata)
      if (!args.dryRun && data.success && data.diff) {
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

      let result = JSON.stringify(data);

      // Append inline diagnostics to output (matching write tool pattern)
      if (!args.dryRun) {
        const diags = data.lsp_diagnostics as Array<Record<string, unknown>> | undefined;
        if (diags && diags.length > 0) {
          const errors = diags.filter((d) => d.severity === "error");
          if (errors.length > 0) {
            const diagLines = errors.map((d) => `  Line ${d.line}: ${d.message}`).join("\n");
            result += `\n\nLSP errors detected, please fix:\n${diagLines}`;
          }
        }
      }

      return result;
    },
  };
}

// ---------------------------------------------------------------------------
// APPLY_PATCH tool
// ---------------------------------------------------------------------------

const APPLY_PATCH_DESCRIPTION = `Use the \`apply_patch\` tool to edit files. Your patch language is a stripped‑down, file‑oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high‑level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).
*** Move to: <path> - after update file header, renames the file.


Example patch:

\`\`\`
*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch
\`\`\`

**Behavior:**
- All file changes are applied with checkpoint-based rollback — if any file fails, previous changes are rolled back (best-effort)
- Files are backed up before modification
- Parent directories are created automatically for new files
- Fuzzy matching for context anchors (handles whitespace and Unicode differences)

**It is important to remember:**

- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with \`+\` even when creating a new file

Returns: Status message string listing created, updated, moved, deleted, or failed file operations. May include inline LSP errors if type errors are introduced by the patch.`;

function createApplyPatchTool(ctx: PluginContext): ToolDefinition {
  return {
    description: APPLY_PATCH_DESCRIPTION,
    args: {
      patchText: z.string().describe("The full patch text including Begin/End markers"),
    },
    execute: async (args, context): Promise<string> => {
      const patchText = args.patchText as string;
      if (!patchText) throw new Error("'patchText' is required");

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

      // Resolve every path this patch touches — SOURCES (h.path) and
      // DESTINATIONS (h.move_path for move hunks). Move destinations have to
      // be tracked because the old code only checkpointed sources; a partial
      // move that succeeded at the destination but failed at source deletion
      // left orphan files behind that rollback never cleaned up (audit #8).
      const affectedAbs = new Set<string>();
      // Files that did NOT exist before this patch — add targets plus move
      // destinations whose path was empty. On rollback we delete these
      // instead of restoring content that was never there.
      const newlyCreatedAbs = new Set<string>();

      for (const h of hunks) {
        const srcAbs = path.resolve(context.directory, h.path);
        affectedAbs.add(srcAbs);
        if (h.type === "add") {
          newlyCreatedAbs.add(srcAbs);
        }
        if (h.type === "update" && h.move_path) {
          const dstAbs = path.resolve(context.directory, h.move_path);
          affectedAbs.add(dstAbs);
          // Snapshot the destination if it exists so rollback restores the
          // original contents. If it doesn't exist, track it as newly
          // created so rollback removes it.
          if (!fs.existsSync(dstAbs)) {
            newlyCreatedAbs.add(dstAbs);
          }
        }
      }

      const relPaths = Array.from(affectedAbs).map((abs) => path.relative(context.worktree, abs));

      await context.ask({
        permission: "edit",
        patterns: relPaths,
        always: ["*"],
        metadata: {},
      });

      // Checkpoint only files that exist pre-patch. Non-existent destinations
      // are tracked in newlyCreatedAbs and reverted by deletion on rollback.
      const checkpointPaths = Array.from(affectedAbs).filter((abs) => !newlyCreatedAbs.has(abs));
      const checkpointName = `apply_patch_${Date.now()}`;
      let checkpointCreated = false;
      if (checkpointPaths.length > 0) {
        try {
          await callBridge(ctx, context, "checkpoint", {
            name: checkpointName,
            files: checkpointPaths,
          });
          checkpointCreated = true;
        } catch {
          // Checkpoint failure is non-fatal — proceed without rollback
          // protection (the hunk loop still records perFileDiffs for the UI).
        }
      }

      // Process each hunk, track per-file diffs for metadata.
      // additions/deletions come from the Rust-side `similar`-crate diff
      // (returned via `include_diff: true` on the write call) — same source
      // as the edit/write tools, which produce correct counts. Avoid
      // recomputing via TS-side LCS to keep one source of truth (issue: the
      // `apply_patch` UI was reporting +N/-N≈filesize counts because the
      // local count was diverging from the Rust truth).
      const results: string[] = [];
      const perFileDiffs: Array<{
        filePath: string;
        before: string;
        after: string;
        additions: number;
        deletions: number;
      }> = [];
      let patchFailed = false;

      for (const hunk of hunks) {
        const filePath = path.resolve(context.directory, hunk.path);

        switch (hunk.type) {
          case "add": {
            try {
              const content = hunk.contents.endsWith("\n") ? hunk.contents : `${hunk.contents}\n`;
              const writeResult = await callBridge(ctx, context, "write", {
                file: filePath,
                content,
                create_dirs: true,
                diagnostics: true,
                include_diff: true,
              });
              const wrDiff = writeResult.diff as
                | { before?: string; after?: string; additions?: number; deletions?: number }
                | undefined;
              perFileDiffs.push({
                filePath,
                before: "",
                after: hunk.contents,
                // For a brand-new file, additions = total lines, deletions = 0.
                // Prefer Rust counts; fall back to a content line count if the
                // bridge didn't include a diff (e.g. older binary).
                additions: wrDiff?.additions ?? lineCount(content),
                deletions: wrDiff?.deletions ?? 0,
              });
              results.push(`Created ${hunk.path}`);
            } catch (e) {
              patchFailed = true;
              results.push(`Failed to create ${hunk.path}: ${e instanceof Error ? e.message : e}`);
            }
            break;
          }

          case "delete": {
            try {
              const before = await fs.promises.readFile(filePath, "utf-8").catch(() => "");
              await callBridge(ctx, context, "delete_file", { file: filePath });
              // delete_file doesn't return a diff. The counts are unambiguous:
              // every prior line is a deletion; nothing is added.
              perFileDiffs.push({
                filePath,
                before,
                after: "",
                additions: 0,
                deletions: lineCount(before),
              });
              results.push(`Deleted ${hunk.path}`);
            } catch (e) {
              patchFailed = true;
              results.push(`Failed to delete ${hunk.path}: ${e instanceof Error ? e.message : e}`);
            }
            break;
          }

          case "update": {
            try {
              // Read original, apply chunks, write back
              const original = await fs.promises.readFile(filePath, "utf-8");
              const newContent = applyUpdateChunks(original, filePath, hunk.chunks);

              const targetPath = hunk.move_path
                ? path.resolve(context.directory, hunk.move_path)
                : filePath;

              const writeResult = await callBridge(ctx, context, "write", {
                file: targetPath,
                content: newContent,
                create_dirs: true,
                diagnostics: true,
                include_diff: true,
              });

              // Collect diagnostics from this file
              const diags = writeResult.lsp_diagnostics as
                | Array<Record<string, unknown>>
                | undefined;
              if (diags && diags.length > 0) {
                const errors = diags.filter((d) => d.severity === "error");
                if (errors.length > 0) {
                  const relPath = path.relative(context.worktree, targetPath);
                  const diagLines = errors.map((d) => `  Line ${d.line}: ${d.message}`).join("\n");
                  results.push(`\nLSP errors detected in ${relPath}, please fix:\n${diagLines}`);
                }
              }

              // Track per-file diff for metadata. For a regular update the
              // Rust write diff compares disk-before vs new content, which
              // matches what we want. For a *move*, write goes to a fresh
              // target (no prior content), so Rust would report the whole
              // file as additions; we recompute via TS-side LCS instead.
              // For non-move updates we still recompute as a fallback when
              // the bridge didn't include a diff (older binary or a test
              // mock without diff support).
              const wrDiff = writeResult.diff as
                | { before?: string; after?: string; additions?: number; deletions?: number }
                | undefined;
              const isMove = Boolean(hunk.move_path);
              const { additions, deletions } =
                isMove || wrDiff?.additions === undefined || wrDiff.deletions === undefined
                  ? countDiffLines(original, newContent)
                  : {
                      additions: wrDiff.additions,
                      deletions: wrDiff.deletions,
                    };
              perFileDiffs.push({
                filePath,
                before: original,
                after: newContent,
                additions,
                deletions,
              });

              if (hunk.move_path) {
                await callBridge(ctx, context, "delete_file", { file: filePath });
                results.push(`Updated and moved ${hunk.path} → ${hunk.move_path}`);
              } else {
                results.push(`Updated ${hunk.path}`);
              }
            } catch (e) {
              patchFailed = true;
              results.push(`Failed to update ${hunk.path}: ${e instanceof Error ? e.message : e}`);
              break;
            }
            break;
          }
        }
      }

      // On failure, restore checkpoint AND delete files that were newly
      // created by this patch (adds + move destinations that didn't exist
      // pre-patch). Checkpoint restore alone only recovers files that
      // existed before — it cannot undo a newly created file or the
      // destination side of a partial move (audit #8).
      if (patchFailed) {
        const rollbackNotes: string[] = [];
        if (checkpointCreated) {
          try {
            await callBridge(ctx, context, "restore_checkpoint", { name: checkpointName });
            rollbackNotes.push("restored pre-existing files from checkpoint");
          } catch {
            rollbackNotes.push("checkpoint restore FAILED, pre-existing files may be inconsistent");
          }
        } else if (checkpointPaths.length > 0) {
          rollbackNotes.push("no checkpoint was created, pre-existing files may be inconsistent");
        }

        // Delete any file we newly created. We call delete_file (which
        // respects validate_path and backs up), and tolerate already-absent
        // files so partial-create failures don't double-error.
        let newlyDeleted = 0;
        for (const createdAbs of newlyCreatedAbs) {
          if (!fs.existsSync(createdAbs)) continue;
          try {
            await callBridge(ctx, context, "delete_file", { file: createdAbs });
            newlyDeleted++;
          } catch {
            rollbackNotes.push(
              `failed to delete newly-created ${path.relative(context.worktree, createdAbs)}`,
            );
          }
        }
        if (newlyDeleted > 0) {
          rollbackNotes.push(`removed ${newlyDeleted} newly-created file(s)`);
        }

        results.push(
          rollbackNotes.length > 0
            ? `Patch failed — ${rollbackNotes.join("; ")}.`
            : "Patch failed — nothing to roll back.",
        );
        return results.join("\n");
      }

      // Store metadata for tool.execute.after hook (match opencode built-in format)
      const callID = getCallID(context);
      if (callID) {
        // Index per-file diffs by absolute filePath for fast lookup when
        // building the metadata.files array. Each entry NEEDS to carry the
        // per-file `patch` string plus `additions`/`deletions` counts —
        // OpenCode's UI patchFile() at packages/ui/src/components/apply-patch-file.ts
        // returns undefined for any file metadata that lacks all of `patch`,
        // `before`, and `after`. Without this enrichment, the UI silently
        // dropped every file entry and rendered no diffs (v0.15.2 fix for
        // the "apply_patch shows no diff in TUI/UI" report).
        const diffByPath = new Map(perFileDiffs.map((d) => [d.filePath, d]));

        // Build per-file metadata. OpenCode's apply_patch shape (see
        // packages/opencode/src/tool/apply_patch.ts:188) per file:
        //   { filePath, relativePath, type, patch, additions, deletions, movePath? }
        // `type` is normalised to "move" when an update hunk has a move target,
        // so the UI can label the row correctly.
        //
        // additions/deletions come from perFileDiffs, which were populated
        // from the Rust-side `similar`-crate diff (via include_diff:true on
        // each write call). This matches edit/write tool counts exactly.
        // The TS-side LCS via buildUnifiedDiff is still used to build the
        // *display* `patch` text — the diff is correct visually; only the
        // line-count derivation through countAddDel was producing wrong
        // numbers (e.g. +399/-400 for a single-line removal). See
        // perFileDiffs population above for how counts are derived per
        // hunk type.
        const files = hunks.map((h) => {
          const filePath = path.resolve(context.directory, h.path);
          // `move_path` only exists on UpdateHunk variants — narrow first.
          const rawMovePath = h.type === "update" ? h.move_path : undefined;
          const movePath = rawMovePath ? path.resolve(context.directory, rawMovePath) : undefined;
          // For moved files, render the destination path as the visible
          // location (matches OpenCode's apply_patch behaviour).
          const displayPath = movePath ?? filePath;
          const relPath = path.relative(context.worktree, displayPath);

          const diffEntry = diffByPath.get(filePath);
          const patch = diffEntry
            ? buildUnifiedDiff(displayPath, diffEntry.before, diffEntry.after)
            : "";
          const additions = diffEntry?.additions ?? 0;
          const deletions = diffEntry?.deletions ?? 0;

          // Normalise type for UI: an "update" hunk with a move target is a
          // move, otherwise keep the parsed type as-is.
          const uiType: "add" | "update" | "delete" | "move" =
            h.type === "update" && rawMovePath ? "move" : h.type;

          return {
            filePath,
            relativePath: relPath,
            type: uiType,
            patch,
            additions,
            deletions,
            ...(movePath ? { movePath } : {}),
          };
        });

        // Build title matching built-in: "Success. Updated the following files:\nM path/to/file.ts"
        const fileList = files
          .map((f) => {
            const prefix = f.type === "add" ? "A" : f.type === "delete" ? "D" : "M";
            return `${prefix} ${f.relativePath}`;
          })
          .join("\n");
        const title = `Success. Updated the following files:\n${fileList}`;

        // Aggregate unified diff for the top-level metadata.diff field
        // (OpenCode's renderer also uses this for some views).
        const diffText = files
          .map((f) => f.patch)
          .filter(Boolean)
          .join("\n");

        storeToolMetadata(context.sessionID, callID, {
          title,
          metadata: {
            diff: diffText,
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
  "Delete a file with backup.\n\n" +
  "The file content is backed up before deletion — use aft_safety undo to recover if needed.";

function createDeleteTool(ctx: PluginContext): ToolDefinition {
  return {
    description: DELETE_DESCRIPTION,
    args: {
      filePath: z.string().describe("Path to file to delete"),
    },
    execute: async (args, context): Promise<string> => {
      const filePath = path.isAbsolute(args.filePath as string)
        ? (args.filePath as string)
        : path.resolve(context.directory, args.filePath as string);

      await context.ask({
        permission: "edit",
        patterns: [filePath],
        always: ["*"],
        metadata: { action: "delete" },
      });

      const result = await callBridge(ctx, context, "delete_file", { file: filePath });
      if (result.success === false) {
        throw new Error((result.message as string) || "delete failed");
      }
      return JSON.stringify(result);
    },
  };
}

// ---------------------------------------------------------------------------
// Move / Rename
// ---------------------------------------------------------------------------

const MOVE_DESCRIPTION =
  "Move or rename a file with backup. Creates parent directories for destination automatically\n" +
  "Note: This moves/renames files at the OS level.";

function createMoveTool(ctx: PluginContext): ToolDefinition {
  return {
    description: MOVE_DESCRIPTION,
    args: {
      filePath: z.string().describe("Source file path to move"),
      destination: z.string().describe("Destination file path"),
    },
    execute: async (args, context): Promise<string> => {
      const filePath = path.isAbsolute(args.filePath as string)
        ? (args.filePath as string)
        : path.resolve(context.directory, args.filePath as string);
      const destPath = path.isAbsolute(args.destination as string)
        ? (args.destination as string)
        : path.resolve(context.directory, args.destination as string);

      await context.ask({
        permission: "edit",
        patterns: [filePath, destPath],
        always: ["*"],
        metadata: { action: "move" },
      });

      const result = await callBridge(ctx, context, "move_file", {
        file: filePath,
        destination: destPath,
      });
      if (result.success === false) {
        throw new Error((result.message as string) || "move failed");
      }
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
    write: createWriteTool(ctx, "edit"),
    edit: createEditTool(ctx, "write"),
    apply_patch: createApplyPatchTool(ctx),
    aft_delete: createDeleteTool(ctx),
    aft_move: createMoveTool(ctx),
  };
}

/**
 * Returns the same tools with aft_ prefix (for when hoisting is disabled).
 */
export function aftPrefixedTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const aftEditTool = createEditTool(ctx, "aft_write");

  return {
    aft_read: createReadTool(ctx),
    aft_write: createWriteTool(ctx, "aft_edit"),
    aft_edit: {
      ...aftEditTool,
      execute: async (args, context): Promise<string> => {
        const argRecord = args as Record<string, unknown>;
        // Legacy back-compat: callers (mostly older tests/integrations) used
        // `{ mode, file, ... }` instead of the current schema. Translate
        // `file` -> `filePath` so the rest of the wrapper sees the modern
        // shape. The current edit tool ignores the `mode` field; we keep it
        // in the args object only so the explicit `mode: "write"` branch
        // below can detect it.
        const normalizedArgs: Record<string, unknown> =
          argRecord.mode !== undefined &&
          argRecord.filePath === undefined &&
          typeof argRecord.file === "string"
            ? { ...argRecord, filePath: argRecord.file }
            : { ...argRecord };

        // Explicit legacy `mode: "write"` — route directly to the Rust
        // `write` command. We do NOT fall through to the modern edit tool
        // here, because the modern tool deliberately rejects content-only
        // calls (the v0.17.2 footgun fix). Legacy `mode: "write"` is an
        // *explicit* whole-file write request, which is fine; the danger is
        // *implicit* whole-file writes where a typo in another mode-selecting
        // param silently degrades into overwrite. Returns the same JSON
        // envelope shape the legacy callers expect (success / file /
        // syntax_valid / etc.), not the human-readable string the modern
        // `write` tool returns.
        if (
          normalizedArgs.mode === "write" &&
          typeof normalizedArgs.filePath === "string" &&
          typeof normalizedArgs.content === "string"
        ) {
          const file = normalizedArgs.filePath as string;
          const filePath = path.isAbsolute(file) ? file : path.resolve(context.directory, file);
          const relPath = path.relative(context.worktree, filePath);
          await context.ask({
            permission: "edit",
            patterns: [relPath],
            always: ["*"],
            metadata: { filepath: filePath },
          });
          const writeParams: Record<string, unknown> = {
            file: filePath,
            content: normalizedArgs.content as string,
            create_dirs: normalizedArgs.create_dirs !== false,
            diagnostics: true,
          };
          if (normalizedArgs.dryRun === true || normalizedArgs.dry_run === true) {
            writeParams.dry_run = true;
          }
          const data = await callBridge(ctx, context, "write", writeParams);
          return JSON.stringify(data);
        }

        return aftEditTool.execute(normalizedArgs, context);
      },
    },
    aft_apply_patch: createApplyPatchTool(ctx),
    aft_delete: createDeleteTool(ctx),
    aft_move: createMoveTool(ctx),
  };
}
