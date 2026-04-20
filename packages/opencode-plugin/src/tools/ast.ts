/**
 * Tool definitions for AST pattern search and replace using ast-grep.
 * Supports meta-variables ($VAR for single node, $$$ for multiple nodes).
 * Patterns must be complete AST nodes (valid code fragments).
 */

import { tool } from "@opencode-ai/plugin";

const z = tool.schema;

import type { ToolDefinition } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";
import {
  askEditPermission,
  resolveAbsolutePath,
  resolveRelativePatterns,
  workspacePattern,
} from "./permissions.js";

/** Show output in opencode UI via metadata callback. */
function showOutputToUser(context: unknown, output: string): void {
  const ctx = context as {
    metadata?: (input: { metadata: { output: string } }) => void | Promise<void>;
  };
  ctx.metadata?.({ metadata: { output } });
}

/** Provide helpful hints when a pattern returns 0 matches. */
function getEmptyResultHint(pattern: string, lang: string): string | null {
  const src = pattern.trim();

  if (lang === "python") {
    if (src.startsWith("class ") && src.endsWith(":")) {
      return `Hint: Python class patterns need a body. Try: "${src.slice(0, -1)}" or "${src}\n    $$$"`;
    }
    if ((src.startsWith("def ") || src.startsWith("async def ")) && src.endsWith(":")) {
      return `Hint: Python function patterns need a body. Try adding "\\n    $$$" after the colon.`;
    }
  }

  if (["javascript", "typescript", "tsx"].includes(lang)) {
    if (/^(export\s+)?(async\s+)?function\s+\$[A-Z_]+\s*$/i.test(src)) {
      return `Hint: Function patterns need params and body. Try: "function $NAME($$$) { $$$ }"`;
    }
  }

  return null;
}

const SUPPORTED_LANGS = ["typescript", "tsx", "javascript", "python", "rust", "go"] as const;

export function astTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const searchTool: ToolDefinition = {
    description:
      "Search code patterns across filesystem using AST-aware matching. Supports 6 languages.\n\n" +
      "Use meta-variables: $VAR matches a single AST node, $$$ matches multiple nodes (variadic).\n" +
      "IMPORTANT: Patterns must be complete AST nodes (valid code fragments).\n" +
      "For functions, include params and body: 'export async function $NAME($$$) { $$$ }' not just 'export async function $NAME'.\n\n" +
      "Examples: pattern='console.log($MSG)' lang='typescript', pattern='async function $NAME($$$) { $$$ }' lang='javascript', pattern='def $FUNC($$$): $$$' lang='python'\n\n" +
      "Returns: Text summary — 'Found N match(es) across M file(s)' followed by file:line blocks with matched text and captured meta-variables.",
    args: {
      pattern: z
        .string()
        .describe("AST pattern with meta-variables ($VAR, $$$). Must be complete AST node."),
      lang: z.enum(SUPPORTED_LANGS).describe("Target language"),
      paths: z.array(z.string()).optional().describe("Paths to search (default: ['.'])"),
      globs: z.array(z.string()).optional().describe("Include/exclude globs (prefix ! to exclude)"),
      contextLines: z
        .number()
        .optional()
        .describe("Number of context lines to show around each match"),
    },
    execute: async (args, context): Promise<string> => {
      const params: Record<string, unknown> = {
        pattern: args.pattern,
        lang: args.lang,
      };
      if (args.paths) params.paths = args.paths;
      if (args.globs) params.globs = args.globs;
      if (args.contextLines !== undefined) params.context = Number(args.contextLines);
      const response = await callBridge(ctx, context, "ast_search", params);

      // Error response (e.g. invalid pattern)
      if (response.success === false) {
        throw new Error((response.message as string) || "ast_search failed");
      }

      // Format output for readability
      const data = response as {
        ok?: boolean;
        matches?: Array<{
          file?: string;
          line?: number;
          text?: string;
          meta_variables?: Record<string, string>;
        }>;
        total_matches?: number;
        files_with_matches?: number;
        files_searched?: number;
      };

      const matchCount = data.total_matches ?? data.matches?.length ?? 0;
      const filesSearched = data.files_searched ?? 0;
      const filesWithMatches = data.files_with_matches ?? filesSearched;

      let output: string;
      if (matchCount === 0) {
        // Zero-match format is intentionally not documented in the description — it's
        // self-explanatory text and documenting it would bloat the Returns section.
        output = `No matches found (searched ${filesSearched} files)`;
        // Add hints for common pattern mistakes
        const hint = getEmptyResultHint(args.pattern as string, args.lang as string);
        if (hint) {
          output += `\n\n${hint}`;
        }
      } else {
        output = `Found ${matchCount} match(es) in ${filesWithMatches} file(s) (${filesSearched} searched)\n\n`;
        if (data.matches) {
          for (const m of data.matches) {
            const relFile = m.file ?? "unknown";
            const line = m.line ?? 0;
            output += `${relFile}:${line}\n`;
            if (m.text) {
              output += `  ${m.text.trim()}\n`;
            }
            if (m.meta_variables && Object.keys(m.meta_variables).length > 0) {
              for (const [k, v] of Object.entries(m.meta_variables)) {
                output += `  ${k}: ${v}\n`;
              }
            }
            output += "\n";
          }
        }
      }

      // Show output in UI
      showOutputToUser(context, output);
      return output;
    },
  };

  const replaceTool: ToolDefinition = {
    description:
      "Replace code patterns across filesystem with AST-aware rewriting. Applies changes by default — set dryRun=true to preview.\n\n" +
      "Use meta-variables in the rewrite pattern to preserve matched content from the pattern.\n" +
      "IMPORTANT: Patterns must be complete AST nodes (valid code fragments).\n\n" +
      "Example: pattern='console.log($MSG)' rewrite='logger.info($MSG)' lang='typescript' — replaces all console.log calls with logger.info across TypeScript files.\n\n" +
      "**Warning: This tool modifies files directly.** Use dryRun=true to preview. Consider creating an aft_safety checkpoint before bulk replacements.\n\n" +
      "Returns: Text summary — 'Replaced N match(es) across M file(s)' (or '[DRY RUN] Would replace...') followed by file:line blocks with before/after text.",
    args: {
      pattern: z
        .string()
        .describe("AST pattern with meta-variables ($VAR, $$$). Must be complete AST node."),
      rewrite: z.string().describe("Replacement pattern (can use $VAR from pattern)"),
      lang: z.enum(SUPPORTED_LANGS).describe("Target language"),
      paths: z.array(z.string()).optional().describe("Paths to search (default: ['.'])"),
      globs: z.array(z.string()).optional().describe("Include/exclude globs (prefix ! to exclude)"),
      dryRun: z.boolean().optional().describe("Preview changes without applying (default: false)"),
    },
    execute: async (args, context): Promise<string> => {
      const isDryRun = args.dryRun === true;

      if (!isDryRun) {
        const explicitPaths = Array.isArray(args.paths)
          ? resolveRelativePatterns(context, args.paths as string[])
          : [];
        const positiveGlobs = Array.isArray(args.globs)
          ? (args.globs as string[]).filter((glob) => !glob.startsWith("!"))
          : [];
        const patterns = [...explicitPaths, ...positiveGlobs];
        const metadata =
          explicitPaths.length === 1 && positiveGlobs.length === 0 && Array.isArray(args.paths)
            ? { filepath: resolveAbsolutePath(context, (args.paths as string[])[0] as string) }
            : {};
        const permissionError = await askEditPermission(
          context,
          patterns.length > 0 ? patterns : [workspacePattern(context)],
          metadata,
        );
        if (permissionError) {
          const output = `Permission denied: ${permissionError}`;
          showOutputToUser(context, output);
          return output;
        }
      }

      const params: Record<string, unknown> = {
        pattern: args.pattern,
        rewrite: args.rewrite,
        lang: args.lang,
      };
      if (args.paths) params.paths = args.paths;
      if (args.globs) params.globs = args.globs;
      params.dry_run = args.dryRun === true;
      const response = await callBridge(ctx, context, "ast_replace", params);

      // Error response (e.g. invalid pattern)
      if (response.success === false) {
        throw new Error((response.message as string) || "ast_replace failed");
      }

      const data = response as {
        ok?: boolean;
        matches?: Array<{ file?: string; line?: number; text?: string; replacement?: string }>;
        total_matches?: number;
        total_replacements?: number;
        total_files?: number;
        files_with_matches?: number;
        files_searched?: number;
      };

      const matchCount = data.total_replacements ?? data.total_matches ?? data.matches?.length ?? 0;
      const filesSearched = data.files_searched ?? data.total_files ?? 0;
      const filesWithMatches = data.files_with_matches ?? data.total_files ?? filesSearched;

      let output: string;
      if (matchCount === 0) {
        output = `No matches found (searched ${filesSearched} files)`;
      } else {
        output = isDryRun
          ? `[DRY RUN] Would replace ${matchCount} match(es) in ${filesWithMatches} file(s) (${filesSearched} searched)\n\n`
          : `Replaced ${matchCount} match(es) in ${filesWithMatches} file(s) (${filesSearched} searched)\n\n`;
        if (data.matches) {
          for (const m of data.matches) {
            const relFile = m.file ?? "unknown";
            const line = m.line ?? 0;
            output += `${relFile}:${line}\n`;
            if (m.text && m.replacement) {
              output += `  - ${m.text.trim()}\n`;
              output += `  + ${m.replacement.trim()}\n`;
            }
            output += "\n";
          }
        }
      }

      showOutputToUser(context, output);
      return output;
    },
  };

  // When hoisting: register as ast_grep_search/ast_grep_replace (override oh-my-opencode's)
  // When not hoisting: register as aft_ast_search/aft_ast_replace
  const hoisting = ctx.config.hoist_builtin_tools !== false;
  return {
    [hoisting ? "ast_grep_search" : "aft_ast_search"]: searchTool,
    [hoisting ? "ast_grep_replace" : "aft_ast_replace"]: replaceTool,
  };
}
