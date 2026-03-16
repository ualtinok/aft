/**
 * Tool definitions for AST pattern search and replace using ast-grep.
 * Supports meta-variables ($VAR for single node, $$$ for multiple nodes).
 * Patterns must be complete AST nodes (valid code fragments).
 */

import { tool } from "@opencode-ai/plugin";

const z = tool.schema;

import type { ToolDefinition } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

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

const SUPPORTED_LANGS = [
  "bash",
  "c",
  "cpp",
  "csharp",
  "css",
  "elixir",
  "go",
  "haskell",
  "html",
  "java",
  "javascript",
  "json",
  "kotlin",
  "lua",
  "nix",
  "php",
  "python",
  "ruby",
  "rust",
  "scala",
  "solidity",
  "swift",
  "typescript",
  "tsx",
  "yaml",
] as const;

export function astTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const searchTool: ToolDefinition = {
    description:
      "Search code patterns across filesystem using AST-aware matching. Supports 25 languages.\n\n" +
      "Use meta-variables: $VAR matches a single AST node, $$$ matches multiple nodes (variadic).\n" +
      "IMPORTANT: Patterns must be complete AST nodes (valid code fragments).\n" +
      "For functions, include params and body: 'export async function $NAME($$$) { $$$ }' not just 'export async function $NAME'.\n\n" +
      "Parameters:\n" +
      "- pattern (string, required): AST pattern with meta-variables. Must be a complete AST node.\n" +
      "- lang (enum, required): Target language — bash, c, cpp, csharp, css, elixir, go, haskell, html, java, javascript, json, kotlin, lua, nix, php, python, ruby, rust, scala, solidity, swift, typescript, tsx, yaml\n" +
      "- paths (string[], optional): Directories or files to search (default: project root)\n" +
      "- globs (string[], optional): Include/exclude glob patterns — prefix '!' to exclude (e.g. ['src/**', '!node_modules'])\n" +
      "- context (number, optional): Number of context lines to show around each match\n\n" +
      "Examples: pattern='console.log($MSG)' lang='typescript', pattern='async function $NAME($$$) { $$$ }' lang='javascript', pattern='def $FUNC($$$): $$$' lang='python'",
    args: {
      pattern: z
        .string()
        .describe("AST pattern with meta-variables ($VAR, $$$). Must be complete AST node."),
      lang: z.enum(SUPPORTED_LANGS).describe("Target language"),
      paths: z.array(z.string()).optional().describe("Paths to search (default: ['.'])"),
      globs: z.array(z.string()).optional().describe("Include/exclude globs (prefix ! to exclude)"),
      context: z.number().optional().describe("Context lines around match"),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const params: Record<string, unknown> = {
        pattern: args.pattern,
        lang: args.lang,
      };
      if (args.paths) params.paths = args.paths;
      if (args.globs) params.globs = args.globs;
      if (args.context !== undefined) params.context = Number(args.context);
      const response = await bridge.send("ast_search", params);

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
        files_searched?: number;
      };

      const matchCount = data.total_matches ?? data.matches?.length ?? 0;
      const filesSearched = data.files_searched ?? 0;

      let output: string;
      if (matchCount === 0) {
        output = `No matches found (searched ${filesSearched} files)`;
        // Add hints for common pattern mistakes
        const hint = getEmptyResultHint(args.pattern as string, args.lang as string);
        if (hint) {
          output += `\n\n${hint}`;
        }
      } else {
        output = `Found ${matchCount} match(es) across ${filesSearched} file(s)\n\n`;
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
      "Replace code patterns across filesystem with AST-aware rewriting. Dry-run by default — set dryRun=false to apply.\n\n" +
      "Use meta-variables in the rewrite pattern to preserve matched content from the pattern.\n\n" +
      "Parameters:\n" +
      "- pattern (string, required): AST pattern to match (same syntax as ast_grep_search)\n" +
      "- rewrite (string, required): Replacement pattern — use $VAR from the match pattern to preserve captured content\n" +
      "- lang (enum, required): Target language — typescript, javascript, tsx, python, rust, go, and 19 more\n" +
      "- paths (string[], optional): Directories or files to search (default: project root)\n" +
      "- globs (string[], optional): Include/exclude glob patterns — prefix '!' to exclude\n" +
      "- dryRun (boolean, optional, default: true): Preview changes without applying. Set to false to apply.\n\n" +
      "Example: pattern='console.log($MSG)' rewrite='logger.info($MSG)' lang='typescript' — replaces all console.log calls with logger.info across TypeScript files.",
    args: {
      pattern: z.string().describe("AST pattern to match"),
      rewrite: z.string().describe("Replacement pattern (can use $VAR from pattern)"),
      lang: z.enum(SUPPORTED_LANGS).describe("Target language"),
      paths: z.array(z.string()).optional().describe("Paths to search"),
      globs: z.array(z.string()).optional().describe("Include/exclude globs"),
      dryRun: z.boolean().optional().describe("Preview changes without applying (default: true)"),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const params: Record<string, unknown> = {
        pattern: args.pattern,
        rewrite: args.rewrite,
        lang: args.lang,
      };
      if (args.paths) params.paths = args.paths;
      if (args.globs) params.globs = args.globs;
      params.dry_run = args.dryRun !== false;
      const response = await bridge.send("ast_replace", params);

      const data = response as {
        ok?: boolean;
        matches?: Array<{ file?: string; line?: number; text?: string; replacement?: string }>;
        total_matches?: number;
        files_searched?: number;
      };

      const isDryRun = args.dryRun !== false;
      const matchCount = data.total_matches ?? data.matches?.length ?? 0;
      const filesSearched = data.files_searched ?? 0;

      let output: string;
      if (matchCount === 0) {
        output = `No matches found (searched ${filesSearched} files)`;
      } else {
        output = isDryRun
          ? `[DRY RUN] Would replace ${matchCount} match(es) across ${filesSearched} file(s)\n\n`
          : `Replaced ${matchCount} match(es) across ${filesSearched} file(s)\n\n`;
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
