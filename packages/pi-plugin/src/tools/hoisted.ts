/**
 * Hoisted tool overrides — replace Pi's built-in read/write/edit/grep with
 * AFT-backed Rust implementations. Registering a tool with the same name as
 * a built-in replaces the built-in entirely.
 *
 * Each tool provides:
 *  - `promptSnippet` / `promptGuidelines`: teach the model our argument shape
 *    in Pi's system prompt (Pi's built-ins use generic one-liners otherwise).
 *  - `renderCall` / `renderResult` for `write` and `edit`: without these,
 *    Pi's ToolExecutionComponent falls back to the *built-in* renderer for
 *    same-named tools, which reads `path` and `edits[]` and garbles our
 *    `filePath` / `oldString` / `newString` output (issue #15).
 *  - Structured `details: { diff, firstChangedLine }` so the rendered diff
 *    also ends up in the agent's message stream, matching Pi's convention.
 *
 * `read` and `grep` keep the default text-only result rendering because our
 * payload (`path`, `pattern`) already aligns with Pi's built-in arg shape.
 */

import { stat } from "node:fs/promises";
import { homedir } from "node:os";
import { resolve } from "node:path";
import {
  type AgentToolResult,
  type ExtensionAPI,
  renderDiff,
  type Theme,
} from "@mariozechner/pi-coding-agent";
import { type Component, Container, Spacer, Text } from "@mariozechner/pi-tui";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";
import { formatDiffForPi } from "./diff-format.js";

/**
 * Local shape for Pi's render context — the real type is exposed by
 * `@mariozechner/pi-coding-agent`'s internals but not publicly exported.
 * We only read `lastComponent` and `isError` here; everything else is ignored.
 */
interface RenderContextLike {
  lastComponent: Component | undefined;
  isError: boolean;
}

const ReadParams = Type.Object({
  path: Type.String({ description: "Path to the file to read (relative or absolute)" }),
  offset: Type.Optional(
    Type.Number({ description: "Line number to start reading from (1-indexed)" }),
  ),
  limit: Type.Optional(Type.Number({ description: "Maximum number of lines to read" })),
});

const WriteParams = Type.Object({
  filePath: Type.String({
    description: "Path to the file to write (absolute or project-relative)",
  }),
  content: Type.String({ description: "Full file contents to write" }),
});

const EditParams = Type.Object({
  filePath: Type.String({ description: "Path to the file to edit" }),
  oldString: Type.Optional(
    Type.String({ description: "Text to find (exact match, fuzzy fallback)" }),
  ),
  newString: Type.Optional(Type.String({ description: "Replacement text (omit to delete match)" })),
  replaceAll: Type.Optional(Type.Boolean({ description: "Replace every occurrence" })),
  occurrence: Type.Optional(
    Type.Number({ description: "0-indexed occurrence when multiple matches exist" }),
  ),
});

const GrepParams = Type.Object({
  pattern: Type.String({ description: "Regex pattern to search for" }),
  path: Type.Optional(Type.String({ description: "Path scope (file or directory)" })),
  include: Type.Optional(
    Type.String({ description: "Glob filter for included files (e.g. '*.ts,*.tsx')" }),
  ),
  caseSensitive: Type.Optional(Type.Boolean({ description: "Case-sensitive matching" })),
  contextLines: Type.Optional(
    Type.Number({ description: "Lines of context before/after each match" }),
  ),
});

export interface ToolSurfaceFlags {
  hoistRead: boolean;
  hoistWrite: boolean;
  hoistEdit: boolean;
  hoistGrep: boolean;
}

/** Details surfaced to both renderer and agent message stream. */
interface FileMutationDetails {
  diff?: string;
  firstChangedLine?: number;
  additions: number;
  deletions: number;
  replacements?: number;
  diagnostics?: unknown[];
  /**
   * True when Rust returned `diff.truncated = true` — the before/after strings
   * were omitted because the file exceeded the diff size cap, so we have no
   * line-level diff to render. Both the agent-facing text and the TUI renderer
   * surface this explicitly rather than silently showing a summary.
   */
  truncated?: boolean;
}

export function registerHoistedTools(
  pi: ExtensionAPI,
  ctx: PluginContext,
  surface: ToolSurfaceFlags,
): void {
  if (surface.hoistRead) {
    pi.registerTool({
      name: "read",
      label: "read",
      description:
        "Read file contents with line numbers. Backed by AFT's indexed Rust reader — faster than the built-in `read` on large repos and correctly handles images/PDFs as attachments.",
      promptSnippet: "Read file contents (supports offset/limit for large files)",
      promptGuidelines: ["Use read to examine files instead of cat or sed."],
      parameters: ReadParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof ReadParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const req: Record<string, unknown> = { file: params.path };
        if (params.offset !== undefined) {
          req.start_line = params.offset;
          if (params.limit !== undefined) {
            req.end_line = params.offset + params.limit - 1;
          }
        } else if (params.limit !== undefined) {
          req.end_line = params.limit;
        }
        const response = await callBridge(bridge, "read", req);
        if (Array.isArray(response.entries)) {
          return textResult((response.entries as string[]).join("\n"));
        }
        const text = (response.content as string | undefined) ?? "";
        return textResult(text);
      },
    });
  }

  if (surface.hoistWrite) {
    pi.registerTool<typeof WriteParams, FileMutationDetails>({
      name: "write",
      label: "write",
      description:
        "Write a file atomically with per-file backup, optional auto-format, and inline LSP diagnostics. Parent directories are created automatically. Overwrites existing files. Uses `filePath` (not `path`).",
      promptSnippet:
        "Create or overwrite files (uses filePath; auto-formats; returns LSP diagnostics inline)",
      promptGuidelines: ["Use write only for new files or complete rewrites."],
      parameters: WriteParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof WriteParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const response = await callBridge(bridge, "write", {
          file: params.filePath,
          content: params.content,
          include_diff: true,
        });
        return buildMutationResult(params.filePath, response);
      },
      renderCall(args, theme, context) {
        return renderMutationCall("write", args?.filePath, theme, context);
      },
      renderResult(result, _options, theme, context) {
        return renderMutationResult(result, theme, context);
      },
    });
  }

  if (surface.hoistEdit) {
    pi.registerTool<typeof EditParams, FileMutationDetails>({
      name: "edit",
      label: "edit",
      description:
        "Find-and-replace edit with progressive fuzzy matching (handles whitespace and Unicode drift). Uses `filePath`, `oldString`, `newString`. Errors on multiple matches — use `occurrence` to pick one, or `replaceAll: true`. Always returns LSP diagnostics inline.",
      promptSnippet:
        "Targeted find-and-replace (uses filePath/oldString/newString; occurrence or replaceAll for disambiguation; fuzzy whitespace matching)",
      promptGuidelines: [
        "Prefer edit over write when changing part of an existing file.",
        "Include enough surrounding context in oldString to make the match unique, or set replaceAll/occurrence explicitly.",
      ],
      parameters: EditParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof EditParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const req: Record<string, unknown> = {
          file: params.filePath,
          match: params.oldString ?? "",
          replacement: params.newString ?? "",
          include_diff: true,
        };
        if (params.replaceAll === true) req.replace_all = true;
        if (params.occurrence !== undefined) req.occurrence = params.occurrence;

        const response = await callBridge(bridge, "edit_match", req);
        return buildMutationResult(params.filePath, response);
      },
      renderCall(args, theme, context) {
        return renderMutationCall("edit", args?.filePath, theme, context);
      },
      renderResult(result, _options, theme, context) {
        return renderMutationResult(result, theme, context);
      },
    });
  }

  if (surface.hoistGrep) {
    pi.registerTool({
      name: "grep",
      label: "grep",
      description:
        "Search for a regex pattern across files. Uses AFT's trigram index inside the project root for fast repeated queries, and falls back to ripgrep for paths outside the project root.",
      promptSnippet: "Fast regex search across files (trigram-indexed inside the project root)",
      promptGuidelines: ["Prefer grep over bash-invoked find/rg for in-project searches."],
      parameters: GrepParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof GrepParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const req: Record<string, unknown> = { pattern: params.pattern };
        if (params.path) req.path = await resolvePathArg(extCtx.cwd, params.path);
        if (params.include) req.include = splitIncludeGlobs(params.include);
        if (params.caseSensitive !== undefined) req.case_sensitive = params.caseSensitive;
        if (params.contextLines !== undefined) req.context_lines = params.contextLines;

        const response = await callBridge(bridge, "grep", req);
        const text = (response.text as string | undefined) ?? "";
        return textResult(text);
      },
    });
  }
}

// ---------------------------------------------------------------------------
// Mutation helpers — write and edit share result shape and rendering.
// ---------------------------------------------------------------------------

/**
 * Shape the bridge `edit_match` / `write` response into an `AgentToolResult`
 * Pi can render. Exported for unit tests covering truncation and diagnostics
 * behavior without spinning up a real bridge.
 */
export function buildMutationResult(
  filePath: string,
  response: Record<string, unknown>,
): AgentToolResult<FileMutationDetails> {
  const diffObj = response.diff as
    | {
        before?: string;
        after?: string;
        additions?: number;
        deletions?: number;
        truncated?: boolean;
      }
    | undefined;
  const additions = diffObj?.additions ?? 0;
  const deletions = diffObj?.deletions ?? 0;
  const replacements = response.replacements as number | undefined;
  const diagnostics = response.lsp_diagnostics as unknown[] | undefined;
  const truncated = diffObj?.truncated === true;

  // Generate the Pi-style line-numbered diff when Rust gave us before/after
  // and the diff wasn't truncated. Truncated diffs carry `additions`/`deletions`
  // counts but no before/after strings, so we surface that explicitly in both
  // the agent-facing text and the TUI renderer instead of silently collapsing
  // to a summary-only output.
  let diffText: string | undefined;
  let firstChangedLine: number | undefined;
  if (
    diffObj &&
    !truncated &&
    typeof diffObj.before === "string" &&
    typeof diffObj.after === "string"
  ) {
    const formatted = formatDiffForPi(diffObj.before, diffObj.after);
    diffText = formatted.diff;
    firstChangedLine = formatted.firstChangedLine;
  }

  // Agent-facing text: summary header + diff (if present) + truncation
  // notice + diagnostics.
  const summaryHeader =
    replacements !== undefined
      ? `Edited ${filePath} (+${additions}/-${deletions}, ${replacements} replacement${replacements === 1 ? "" : "s"})`
      : `Wrote ${filePath} (+${additions}/-${deletions})`;
  let text = summaryHeader;
  if (diffText) text += `\n\n${diffText}`;
  if (truncated) {
    text += "\n\n(diff truncated \u2014 file too large to include before/after content)";
  }
  if (diagnostics && diagnostics.length > 0) {
    text += `\n\nLSP diagnostics:\n${formatDiagnosticsText(diagnostics)}`;
  }

  return {
    content: [{ type: "text", text }],
    details: {
      diff: diffText,
      firstChangedLine,
      additions,
      deletions,
      replacements,
      diagnostics,
      truncated: truncated || undefined,
    },
  };
}

function formatDiagnosticsText(diagnostics: unknown[]): string {
  // Diagnostics come back as an array of { line, severity, message, ... }.
  // Keep the format compact and human-readable; fall back to JSON if shape
  // is unexpected.
  try {
    return diagnostics
      .map((d) => {
        if (d && typeof d === "object") {
          const obj = d as Record<string, unknown>;
          const line = obj.line ?? obj.startLine ?? "?";
          const severity = obj.severity ?? "info";
          const msg = obj.message ?? JSON.stringify(obj);
          return `  [${severity}] line ${line}: ${msg}`;
        }
        return `  ${String(d)}`;
      })
      .join("\n");
  } catch {
    return JSON.stringify(diagnostics, null, 2);
  }
}

/**
 * Reuse a compatible `Text` from `lastComponent`, or create a fresh one.
 * The runtime `instanceof` guard prevents a cross-branch re-render from
 * trying to use a `Container` as a `Text` (or vice versa) — today Pi keeps
 * call/result slots separate and each slot's branch is stable per call, so
 * this is defensive hardening rather than a current-bug fix.
 */
function reuseText(last: Component | undefined): Text {
  return last instanceof Text ? last : new Text("", 0, 0);
}

function reuseContainer(last: Component | undefined): Container {
  return last instanceof Container ? last : new Container();
}

function renderMutationCall(
  toolName: "write" | "edit",
  filePath: string | undefined,
  theme: Theme,
  context: RenderContextLike,
): Text {
  const text = reuseText(context.lastComponent);
  const pathDisplay = filePath
    ? theme.fg("accent", shortenPath(filePath))
    : theme.fg("toolOutput", "...");
  text.setText(`${theme.fg("toolTitle", theme.bold(toolName))} ${pathDisplay}`);
  return text;
}

function renderMutationResult(
  result: AgentToolResult<FileMutationDetails>,
  theme: Theme,
  context: RenderContextLike,
): Container | Text {
  // Errors: red text.
  if (context.isError) {
    const errorText = result.content
      .filter((c) => c.type === "text")
      .map((c) => (c as { text?: string }).text ?? "")
      .join("\n")
      .trim();
    const text = reuseText(context.lastComponent);
    text.setText(`\n${theme.fg("error", errorText || "edit failed")}`);
    return text;
  }

  const details = result.details;
  const diff = typeof details?.diff === "string" ? details.diff : undefined;

  // No diff (no-op edit or truncated diff): one-line summary. Truncation is
  // surfaced explicitly in muted text so the user isn't misled into thinking
  // a tiny summary reflects a tiny change.
  if (!diff) {
    const additions = details?.additions ?? 0;
    const deletions = details?.deletions ?? 0;
    const text = reuseText(context.lastComponent);
    const summary = theme.fg("success", `+${additions}/-${deletions}`);
    const suffix = details?.truncated ? ` ${theme.fg("muted", "(diff truncated)")}` : "";
    text.setText(`\n${summary}${suffix}`);
    return text;
  }

  // Diff: render using Pi's built-in renderer for colored lines + intra-line
  // highlighting, wrapped in a Container with a top spacer for breathing room.
  const container = reuseContainer(context.lastComponent);
  container.clear();
  container.addChild(new Spacer(1));
  container.addChild(new Text(renderDiff(diff), 1, 0));
  return container;
}

function shortenPath(path: string): string {
  const home = homedir();
  if (path.startsWith(home)) return `~${path.slice(home.length)}`;
  return path;
}

/** Resolve a path argument to an absolute path if it exists. */
async function resolvePathArg(cwd: string, path: string): Promise<string> {
  const abs = resolve(cwd, path);
  try {
    await stat(abs);
    return abs;
  } catch {
    return path;
  }
}

/** Split OpenCode-style include args like `"*.ts,*.tsx"` into a glob array. */
function splitIncludeGlobs(include: string): string[] {
  return include
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}
