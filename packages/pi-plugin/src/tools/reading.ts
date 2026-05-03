/**
 * AFT reading tools: aft_outline + aft_zoom.
 * Structural overview and symbol/section inspection.
 */

import { stat } from "node:fs/promises";
import { resolve } from "node:path";
import type { AgentToolResult, ExtensionAPI, Theme } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import { discoverSourceFiles } from "../shared/discover-files.js";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";
import {
  accentPath,
  asRecord,
  asRecords,
  asString,
  collectTextContent,
  extractStructuredPayload,
  type RenderContextLike,
  renderErrorResult,
  renderSections,
  renderToolCall,
  shortenPath,
} from "./render-helpers.js";

const OutlineParams = Type.Object({
  target: Type.Union([Type.String(), Type.Array(Type.String())], {
    description:
      "What to outline: a file path, directory path, or array of file paths. The mode is auto-detected: directories by stat, arrays as multi-file. Directory walks cap at 200 files.",
  }),
});

const ZoomParams = Type.Object({
  filePath: Type.String({ description: "Path to file (absolute or project-relative)" }),
  symbol: Type.Optional(
    Type.String({ description: "Symbol name (function/class/type) or Markdown heading" }),
  ),
  symbols: Type.Optional(
    Type.Array(Type.String(), { description: "Multiple symbols — returns array of matches" }),
  ),
  contextLines: Type.Optional(
    Type.Number({ description: "Lines of context before/after (default: 3)" }),
  ),
});

export interface ReadingSurface {
  outline: boolean;
  zoom: boolean;
}

interface ZoomBatchSymbolResult {
  name: string;
  success: boolean;
  content?: string;
  error?: string;
}

interface ZoomBatchResult {
  complete: boolean;
  symbols: ZoomBatchSymbolResult[];
  text: string;
}

/** Exported for renderer unit tests. */
export function buildOutlineSections(text: string, theme: Theme): string[] {
  const trimmed = text.trim();
  if (!trimmed) return [theme.fg("muted", "No outline available.")];

  const lines = trimmed.split("\n");
  if (lines.length === 1) return [theme.fg("accent", lines[0])];
  return [theme.fg("accent", lines[0]), lines.slice(1).join("\n")];
}

/** Exported for renderer unit tests. */
export function buildZoomSections(
  args: Static<typeof ZoomParams>,
  payload: unknown,
  theme: Theme,
): string[] {
  const batch = asRecord(payload);
  if (Array.isArray(batch?.symbols)) {
    const header = batch.complete === false ? [theme.fg("warning", "Incomplete zoom results")] : [];
    const items = batch.symbols as unknown[];
    return [
      ...header,
      ...items.map((item) => {
        const record = asRecord(item);
        if (!record) return theme.fg("muted", "No zoom result available.");
        const name = asString(record.name) ?? "(unknown symbol)";
        if (record.success === false) {
          return theme.fg(
            "error",
            `Symbol "${name}" not found: ${asString(record.error) ?? "zoom failed"}`,
          );
        }
        const content = asString(record.content);
        return [
          `${theme.fg("accent", name)} ${theme.fg("muted", shortenPath(args.filePath))}`,
          content,
        ]
          .filter(Boolean)
          .join("\n");
      }),
    ];
  }

  const items = Array.isArray(payload) ? payload : payload ? [payload] : [];
  if (items.length === 0) return [theme.fg("muted", "No zoom result available.")];

  return items
    .map((item) => {
      const record = asRecord(item);
      if (!record) return theme.fg("muted", "No zoom result available.");

      const name = asString(record.name) ?? "(unknown symbol)";
      const kind = asString(record.kind) ?? "symbol";
      const range = asRecord(record.range);
      const startLine =
        range && typeof range.start_line === "number" ? range.start_line : undefined;
      const endLine = range && typeof range.end_line === "number" ? range.end_line : undefined;
      const location =
        startLine !== undefined
          ? `${shortenPath(args.filePath)}:${startLine}${endLine && endLine !== startLine ? `-${endLine}` : ""}`
          : shortenPath(args.filePath);
      const lines = [`${theme.fg("accent", name)} ${theme.fg("muted", `[${kind}] ${location}`)}`];

      const content = asString(record.content);
      if (content) {
        lines.push(
          content
            .split("\n")
            .map((line) => `  ${line}`)
            .join("\n"),
        );
      }

      const annotations = asRecord(record.annotations);
      const callsOut = annotations ? asRecords(annotations.calls_out) : [];
      const calledBy = annotations ? asRecords(annotations.called_by) : [];
      if (callsOut.length > 0) {
        lines.push(
          `${theme.fg("muted", "calls out")}`,
          callsOut
            .map(
              (call) =>
                `  ↳ ${asString(call.name) ?? "(unknown)"}${typeof call.line === "number" ? `:${call.line}` : ""}`,
            )
            .join("\n"),
        );
      }
      if (calledBy.length > 0) {
        lines.push(
          `${theme.fg("muted", "called by")}`,
          calledBy
            .map(
              (call) =>
                `  ↳ ${asString(call.name) ?? "(unknown)"}${typeof call.line === "number" ? `:${call.line}` : ""}`,
            )
            .join("\n"),
        );
      }

      return lines.join("\n");
    })
    .filter(Boolean);
}

/** Exported for renderer unit tests. */
export function renderOutlineCall(
  args: Static<typeof OutlineParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  const summary = Array.isArray(args.target)
    ? theme.fg("accent", `${args.target.length} files`)
    : typeof args.target === "string"
      ? accentPath(theme, args.target)
      : undefined;
  return renderToolCall("outline", summary, theme, context);
}

/** Exported for renderer unit tests. */
export function renderOutlineResult(
  result: AgentToolResult<unknown>,
  theme: Theme,
  context: RenderContextLike,
) {
  if (context.isError) return renderErrorResult(result, "outline failed", theme, context);
  return renderSections(buildOutlineSections(collectTextContent(result), theme), context);
}

/** Exported for renderer unit tests. */
export function renderZoomCall(
  args: Static<typeof ZoomParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  const target = args.symbol
    ? theme.fg("toolOutput", args.symbol)
    : args.symbols && args.symbols.length > 0
      ? theme.fg("toolOutput", `${args.symbols.length} symbols`)
      : theme.fg("toolOutput", "lines");
  return renderToolCall("zoom", `${accentPath(theme, args.filePath)} ${target}`, theme, context);
}

/** Exported for renderer unit tests. */
export function renderZoomResult(
  result: AgentToolResult<unknown>,
  args: Static<typeof ZoomParams>,
  theme: Theme,
  context: RenderContextLike,
) {
  if (context.isError) return renderErrorResult(result, "zoom failed", theme, context);
  return renderSections(buildZoomSections(args, extractStructuredPayload(result), theme), context);
}

export function registerReadingTools(
  pi: ExtensionAPI,
  ctx: PluginContext,
  surface: ReadingSurface,
): void {
  if (surface.outline) {
    pi.registerTool({
      name: "aft_outline",
      label: "outline",
      description:
        "Structural outline of source code or Markdown. For code, returns symbols (functions, classes, types) with line ranges. For Markdown/HTML, returns heading hierarchy. Use this to explore structure before reading specific sections with aft_zoom.\n\nPass a single `target`:\n  • file path → outline that file (with signatures)\n  • directory path → outline source files under it (recursively, up to 200 files)\n  • array of paths → outline multiple files in one call",
      parameters: OutlineParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof OutlineParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const target = params.target;
        const isArray = Array.isArray(target) && target.length > 0;

        // Multi-file mode
        if (isArray) {
          const response = await callBridge(
            bridge,
            "outline",
            { files: target as string[] },
            extCtx,
          );
          return textResult(formatOutlineText(response));
        }

        if (typeof target !== "string" || target.length === 0) {
          throw new Error("'target' must be a non-empty string or array of strings");
        }

        // Stat to disambiguate file vs directory
        let isDirectory = false;
        try {
          const resolved = resolve(extCtx.cwd, target);
          const st = await stat(resolved);
          isDirectory = st.isDirectory();
        } catch {
          // path doesn't exist locally — fall through to single-file mode and let
          // Rust report the real error
        }

        if (isDirectory) {
          const dirPath = resolve(extCtx.cwd, target);
          const files = await discoverSourceFiles(dirPath);
          if (files.length === 0) {
            return textResult(`No source files found under ${target}`);
          }
          const response = await callBridge(bridge, "outline", { files }, extCtx);
          return textResult(formatOutlineText(response));
        }

        const response = await callBridge(bridge, "outline", { file: target }, extCtx);
        return textResult(formatOutlineText(response));
      },
      renderCall(args, theme, context) {
        return renderOutlineCall(args, theme, context);
      },
      renderResult(result, _options, theme, context) {
        return renderOutlineResult(result, theme, context);
      },
    });
  }

  if (surface.zoom) {
    pi.registerTool({
      name: "aft_zoom",
      label: "zoom",
      description:
        "Inspect a code symbol or Markdown/HTML section. For code, returns the full source of the symbol with call-graph annotations (calls/called-by). Pass `symbols` for batched lookups.",
      parameters: ZoomParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof ZoomParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);

        // Multi-symbol: fire in parallel and preserve per-symbol failures.
        // Uses callBridge (not bridge.send directly) so each parallel request
        // carries Pi's native session_id — otherwise multi-symbol zoom would
        // bypass per-session undo/checkpoint scoping.
        if (Array.isArray(params.symbols) && params.symbols.length > 0) {
          const results = await Promise.all(
            params.symbols.map((sym) => {
              const req: Record<string, unknown> = { file: params.filePath, symbol: sym };
              if (params.contextLines !== undefined) req.context_lines = params.contextLines;
              return callBridge(bridge, "zoom", req, extCtx).catch((err) => ({
                success: false,
                message: err instanceof Error ? err.message : String(err),
              }));
            }),
          );
          const batch = formatZoomBatchResult(params.symbols, results);
          return textResult(batch.text, batch);
        }

        const req: Record<string, unknown> = { file: params.filePath };
        if (params.symbol) req.symbol = params.symbol;
        if (params.contextLines !== undefined) req.context_lines = params.contextLines;
        const response = await callBridge(bridge, "zoom", req, extCtx);
        return textResult(JSON.stringify(response, null, 2));
      },
      renderCall(args, theme, context) {
        return renderZoomCall(args, theme, context);
      },
      renderResult(result, _options, theme, context) {
        return renderZoomResult(result, context.args, theme, context);
      },
    });
  }
}

/** Exported for regression tests. */
export function formatZoomBatchResult(
  symbols: string[],
  responses: Record<string, unknown>[],
): ZoomBatchResult {
  const entries = symbols.map((name, index): ZoomBatchSymbolResult => {
    const response = responses[index] ?? { success: false, message: "missing zoom response" };
    if (response.success === false) {
      const message =
        typeof response.message === "string" && response.message.length > 0
          ? response.message
          : "zoom failed";
      return { name, success: false, error: message };
    }

    return { name, success: true, content: zoomResponseContent(response) };
  });
  const complete = entries.every((entry) => entry.success);
  const lines: string[] = [];
  if (!complete) {
    lines.push("Incomplete zoom results: one or more symbols failed.");
  }
  for (const entry of entries) {
    if (entry.success) {
      lines.push(`Symbol "${entry.name}":\n${entry.content ?? ""}`.trimEnd());
    } else {
      lines.push(`Symbol "${entry.name}" not found: ${entry.error ?? "zoom failed"}`);
    }
  }
  return { complete, symbols: entries, text: lines.join("\n\n") };
}

function zoomResponseContent(response: Record<string, unknown>): string {
  if (typeof response.content === "string") return response.content;
  if (typeof response.text === "string") return response.text;
  const { success: _success, ...rest } = response;
  return JSON.stringify(rest, null, 2);
}

/**
 * Format an outline response into agent-readable text, appending honest skip
 * reporting when files were intentionally skipped (parse error, unsupported
 * language, file not found, too large). Without this, agents only see the tree
 * and assume all input files were processed.
 */
interface SkippedOutlineFile {
  file: string;
  reason: string;
}

function formatOutlineText(response: Record<string, unknown>): string {
  const text = (response.text as string | undefined) ?? "";
  const skipped = response.skipped_files as SkippedOutlineFile[] | undefined;
  if (!skipped || skipped.length === 0) {
    return text;
  }
  const lines = skipped.map(({ file, reason }) => `  ${file} — ${reason}`).join("\n");
  const header = text.length > 0 ? `${text}\n\n` : "";
  return `${header}Skipped ${skipped.length} file(s):\n${lines}`;
}
