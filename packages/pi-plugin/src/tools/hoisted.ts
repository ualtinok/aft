/**
 * Hoisted tool overrides — replace Pi's built-in read/write/edit/grep with
 * AFT-backed Rust implementations. Registering a tool with the same name as
 * a built-in replaces the built-in entirely.
 */

import { stat } from "node:fs/promises";
import { resolve } from "node:path";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

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
      parameters: ReadParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof ReadParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        // Translate OpenCode-style offset/limit into start_line/end_line.
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
        // Directory listings come back as { entries: [...] }, files as { content: "..." }.
        if (Array.isArray(response.entries)) {
          return textResult((response.entries as string[]).join("\n"));
        }
        const text = (response.content as string | undefined) ?? "";
        return textResult(text);
      },
    });
  }

  if (surface.hoistWrite) {
    pi.registerTool({
      name: "write",
      label: "write",
      description:
        "Write a file atomically with per-file backup, optional auto-format, and inline LSP diagnostics. Parent directories are created automatically. Overwrites existing files.",
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
        });
        const diffAdd = (response.diff as { additions?: number } | undefined)?.additions ?? 0;
        const diffDel = (response.diff as { deletions?: number } | undefined)?.deletions ?? 0;
        const diagnostics = response.lsp_diagnostics as unknown[] | undefined;
        let summary = `Wrote ${params.filePath} (+${diffAdd}/-${diffDel})`;
        if (diagnostics && diagnostics.length > 0) {
          summary += `\n\nLSP diagnostics:\n${JSON.stringify(diagnostics, null, 2)}`;
        }
        return textResult(summary, {
          filePath: params.filePath,
          additions: diffAdd,
          deletions: diffDel,
          diagnostics,
        });
      },
    });
  }

  if (surface.hoistEdit) {
    pi.registerTool({
      name: "edit",
      label: "edit",
      description:
        "Find-and-replace edit with progressive fuzzy matching (handles whitespace and Unicode drift). Returns an error if multiple matches are found — use `occurrence` to select one, or `replaceAll: true` to replace all. Always returns inline LSP diagnostics for the edited file.",
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
        };
        if (params.replaceAll === true) req.replace_all = true;
        if (params.occurrence !== undefined) req.occurrence = params.occurrence;

        const response = await callBridge(bridge, "edit_match", req);
        const diff = response.diff as { additions?: number; deletions?: number } | undefined;
        const diffAdd = diff?.additions ?? 0;
        const diffDel = diff?.deletions ?? 0;
        const replacements = response.replacements as number | undefined;
        const diagnostics = response.lsp_diagnostics as unknown[] | undefined;
        let summary = `Edited ${params.filePath} (+${diffAdd}/-${diffDel}, ${replacements ?? 1} replacement${replacements === 1 ? "" : "s"})`;
        if (diagnostics && diagnostics.length > 0) {
          summary += `\n\nLSP diagnostics:\n${JSON.stringify(diagnostics, null, 2)}`;
        }
        return textResult(summary, {
          filePath: params.filePath,
          additions: diffAdd,
          deletions: diffDel,
          replacements,
          diagnostics,
        });
      },
    });
  }

  if (surface.hoistGrep) {
    pi.registerTool({
      name: "grep",
      label: "grep",
      description:
        "Search for a regex pattern across files. Uses AFT's trigram index inside the project root for fast repeated queries, and falls back to ripgrep for paths outside the project root.",
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
