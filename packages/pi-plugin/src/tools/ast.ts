/**
 * ast_grep_search + ast_grep_replace — AST-aware pattern search/rewrite.
 * 6 languages: typescript, tsx, javascript, python, rust, go.
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const AstLang = StringEnum(["typescript", "tsx", "javascript", "python", "rust", "go"] as const, {
  description: "Target language",
});

const SearchParams = Type.Object({
  pattern: Type.String({
    description:
      "AST pattern with meta-variables (`$VAR` matches one node, `$$$` matches many). Must be a complete AST node.",
  }),
  lang: AstLang,
  paths: Type.Optional(
    Type.Array(Type.String(), { description: "Paths to search (default: ['.'])" }),
  ),
  globs: Type.Optional(
    Type.Array(Type.String(), { description: "Include/exclude globs (prefix `!` to exclude)" }),
  ),
  contextLines: Type.Optional(
    Type.Number({ description: "Number of context lines around each match" }),
  ),
});

const ReplaceParams = Type.Object({
  pattern: Type.String({ description: "AST pattern with meta-variables" }),
  rewrite: Type.String({ description: "Replacement pattern, can reference $VAR from pattern" }),
  lang: AstLang,
  paths: Type.Optional(Type.Array(Type.String(), { description: "Paths (default: ['.'])" })),
  globs: Type.Optional(Type.Array(Type.String(), { description: "Include/exclude globs" })),
  dryRun: Type.Optional(Type.Boolean({ description: "Preview without applying (default: false)" })),
});

export interface AstSurface {
  astSearch: boolean;
  astReplace: boolean;
}

export function registerAstTools(pi: ExtensionAPI, ctx: PluginContext, surface: AstSurface): void {
  if (surface.astSearch) {
    pi.registerTool({
      name: "ast_grep_search",
      label: "ast search",
      description:
        "Search code patterns across the filesystem using AST-aware matching. Use `$VAR` to match a single AST node, `$$$` for multiple. Pattern must be a complete, valid code fragment (include braces, params, etc.).",
      parameters: SearchParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof SearchParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const req: Record<string, unknown> = {
          pattern: params.pattern,
          lang: params.lang,
        };
        if (params.paths !== undefined) req.paths = params.paths;
        if (params.globs !== undefined) req.globs = params.globs;
        if (params.contextLines !== undefined) req.context_lines = params.contextLines;
        const response = await callBridge(bridge, "ast_search", req);
        return textResult((response.text as string | undefined) ?? JSON.stringify(response));
      },
    });
  }

  if (surface.astReplace) {
    pi.registerTool({
      name: "ast_grep_replace",
      label: "ast replace",
      description:
        "Replace code patterns across the filesystem with AST-aware rewriting. Applies by default — pass `dryRun: true` to preview. Use meta-variables in `rewrite` to preserve captured content from the pattern.",
      parameters: ReplaceParams,
      async execute(
        _toolCallId: string,
        params: Static<typeof ReplaceParams>,
        _signal,
        _onUpdate,
        extCtx,
      ) {
        const bridge = bridgeFor(ctx, extCtx.cwd);
        const req: Record<string, unknown> = {
          pattern: params.pattern,
          rewrite: params.rewrite,
          lang: params.lang,
        };
        if (params.paths !== undefined) req.paths = params.paths;
        if (params.globs !== undefined) req.globs = params.globs;
        // Rust ast_replace defaults to dry_run=true; apply by default to match description.
        req.dry_run = params.dryRun === true;
        const response = await callBridge(bridge, "ast_replace", req);
        return textResult((response.text as string | undefined) ?? JSON.stringify(response));
      },
    });
  }
}
