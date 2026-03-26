<p align="center">
  <img src="assets/banner.jpeg" alt="AFT — Agent File Toolkit" width="50%" />
</p>

<h1 align="center">AFT — Agent File Toolkit</h1>

<p align="center">
  <strong>Tree-sitter powered code analysis tools for AI coding agents.</strong><br>
  Semantic editing, call-graph navigation, and structural search — all in one toolkit.
</p>

<p align="center">
  <a href="https://crates.io/crates/agent-file-tools"><img src="https://img.shields.io/crates/v/agent-file-tools?label=crate&color=blue&style=flat-square" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/@cortexkit/aft-opencode"><img src="https://img.shields.io/npm/v/@cortexkit/aft-opencode?color=blue&style=flat-square" alt="npm"></a>
  <a href="https://github.com/cortexkit/aft/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT License"></a>
</p>

<p align="center">
  <a href="#get-started">Get Started</a> ·
  <a href="#what-is-aft">What is AFT?</a> ·
  <a href="#features">Features</a> ·
  <a href="#tool-reference">Tool Reference</a> ·
  <a href="#configuration">Configuration</a> ·
  <a href="#architecture">Architecture</a>
</p>

---

## Get Started

### OpenCode

Add AFT to your OpenCode config:

```json
// ~/.config/opencode/config.json
{
  "plugins": ["@cortexkit/aft-opencode@latest"]
}
```

That's it. On the next session start, the binary downloads if needed and all tools become
available. AFT replaces opencode's built-in `read`, `write`, `edit`, `apply_patch`,
`ast_grep_search`, `ast_grep_replace`, and `lsp_diagnostics` with enhanced versions — all
powered natively by AFT — plus adds the `aft_` family of semantic tools on top.

---

## What is AFT?

AI coding agents are fast, but their interaction with code is often blunt. The typical pattern:
read an entire file to find one function, construct a diff from memory, apply it by line number,
and hope nothing shifted. Tokens burned on context noise. Edits that break when the file changes.
Navigation that requires reading three files to answer "what calls this?"

AFT is a toolkit built on top of tree-sitter's concrete syntax trees. Every operation addresses
code by what it *is* — a function, a class, a call site, a symbol — not by where it happens to
sit in a file right now. Agents can outline a file's structure in one call, zoom into a single
function, edit it by name, then follow its callers across the workspace. All without reading a
single line they don't need.

AFT **hoists** itself into opencode's built-in tool slots. The `read`, `write`, `edit`,
`apply_patch`, `ast_grep_search`, `ast_grep_replace`, and `lsp_diagnostics` tools are replaced
by AFT-enhanced versions — same names the agent already knows, but now backed by the Rust binary
for backups, formatting, inline diagnostics, and symbol-aware operations.

The toolkit is a two-component system: a Rust binary that does the heavy lifting (parsing,
analysis, edits, formatting) and a TypeScript plugin that integrates with OpenCode. The binary
ships pre-built for all major platforms and downloads automatically on first use — no install
ceremony required.

---

## How it Helps Agents

**The token problem.** A 500-line file costs ~375 tokens to read. Most of the time, the agent
needs one function. `aft_zoom` with a `symbol` param returns that function plus a few lines of
context: ~40 tokens. Over a multi-step task, the savings compound fast.

**The fragile-edit problem.** Line-number edits break the moment anything above the target moves.
`edit` in symbol mode addresses the function by name. The agent writes the new body; AFT finds
the symbol, replaces it, validates syntax, and runs the formatter. Nothing to count.

**The navigation problem.** "Where is this function called?" means grep or reading every importer.
`aft_navigate` with `callers` mode returns every call site across the workspace in one round trip.
`impact` mode goes further: it tells the agent what else breaks if that function's signature changes.

Here's a typical agent workflow:

**1. Get the file structure:**

```json
// aft_outline
{ "filePath": "src/auth/session.ts" }
```

**2. Read the specific function:**

```json
// aft_zoom
{ "filePath": "src/auth/session.ts", "symbol": "validateToken" }
```

**3. Edit it by name:**

```json
// edit
{
  "filePath": "src/auth/session.ts",
  "symbol": "validateToken",
  "content": "export function validateToken(token: string): boolean {\n  if (!token) return false;\n  return verifyJwt(token);\n}"
}
```

**4. Check who calls it before changing its signature:**

```json
// aft_navigate
{ "op": "callers", "filePath": "src/auth/session.ts", "symbol": "validateToken", "depth": 2 }
```

---

## Features

- **File read** — line-numbered file content, directory listing, and image/PDF detection
- **Semantic outline** — list all symbols in a file (or several files, or a directory) with kind, name, line range, visibility
- **Symbol editing** — replace a named symbol by name with auto-format and syntax validation
- **Match editing** — find-and-replace by content with fuzzy fallback (4-pass: exact → trim trailing → trim both → normalize Unicode)
- **Batch & transaction edits** — atomic multi-edit within a file, or atomic multi-file edits with rollback
- **Glob replace** — pattern replace across all matching files in one call
- **Patch apply** — multi-file `*** Begin Patch` format for creates, updates, deletes, and moves
- **Call tree & callers** — forward call graph and reverse lookup across the workspace
- **Trace-to & impact analysis** — how does execution reach this function? what breaks if it changes?
- **Data flow tracing** — follow a value through assignments and parameters across files
- **Auto-format & auto-backup** — every edit formats the file and saves a snapshot for undo
- **Import management** — add, remove, organize imports language-aware (TS/JS/TSX/Python/Rust/Go)
- **Structural transforms** — add class members, Rust derive macros, Python decorators, Go struct tags, wrap try/catch
- **Workspace-wide refactoring** — move symbols between files (updates all imports), extract functions, inline functions
- **Safety & recovery** — undo last edit, named checkpoints, restore to any checkpoint
- **AST pattern search & replace** — structural code search using meta-variables (`$VAR`, `$$$`), powered by ast-grep
- **Inline diagnostics** — write and edit return LSP errors detected after the change
- **UI metadata** — the OpenCode desktop shows file paths and diff previews (`+N/-N`) for every edit
- **Local tool discovery** — finds biome, prettier, tsc, pyright in `node_modules/.bin` automatically

---

## Tool Reference

> **All line numbers are 1-based** (matching editor, git, and compiler conventions).
> Line 1 is the first line of the file.

### Hoisted tools

These replace opencode's built-ins. Registered under the same names by default. When
`hoist_builtin_tools: false`, they get the `aft_` prefix instead (e.g. `aft_read`).

| Tool | Replaces | Description | Key Params |
|------|----------|-------------|------------|
| `read` | opencode read | File read, directory listing, image/PDF detection | `filePath`, `startLine`, `endLine`, `offset`, `limit` |
| `write` | opencode write | Write file with auto-dirs, backup, format, inline diagnostics | `filePath`, `content` |
| `edit` | opencode edit | Find/replace, symbol replace, batch, transaction, glob | `filePath`, `oldString`, `newString`, `symbol`, `content`, `edits[]` |
| `apply_patch` | opencode apply_patch | `*** Begin Patch` multi-file patch format | `patchText` |
| `ast_grep_search` | oh-my-opencode ast_grep | AST pattern search with meta-variables | `pattern`, `lang`, `paths[]`, `globs[]` |
| `ast_grep_replace` | oh-my-opencode ast_grep | AST pattern replace (applies by default) | `pattern`, `rewrite`, `lang`, `dryRun` |
| `lsp_diagnostics` | opencode lsp_diagnostics | Errors/warnings from language server | `filePath`, `directory`, `severity`, `waitMs` |

### AFT-only tools

Always registered with `aft_` prefix regardless of hoisting setting.

**Recommended tier** (default):

| Tool | Description | Key Params |
|------|-------------|------------|
| `aft_outline` | Structural outline of a file, files, or directory | `filePath`, `files[]`, `directory` |
| `aft_zoom` | Inspect symbols with call-graph annotations | `filePath`, `symbol`, `symbols[]` |
| `aft_import` | Language-aware import add/remove/organize | `op`, `filePath`, `module`, `names[]` |
| `aft_safety` | Undo, history, checkpoints, restore | `op`, `filePath`, `name` |

**All tier** (set `tool_surface: "all"`):

| Tool | Description | Key Params |
|------|-------------|------------|
| `aft_delete` | Delete a file with backup | `filePath` |
| `aft_move` | Move or rename a file with backup | `filePath`, `destination` |
| `aft_navigate` | Call graph and data-flow navigation | `op`, `filePath`, `symbol`, `depth` |
| `aft_transform` | Structural code transforms (members, derives, decorators) | `op`, `filePath`, `container`, `target` |
| `aft_refactor` | Workspace-wide move, extract, inline | `op`, `filePath`, `symbol`, `destination` |

---

### read

Plain file reading and directory listing. Pass `filePath` to read a file, or a directory path to
list its entries. Paginate large files with `startLine`/`endLine` or `offset`/`limit`.

```json
// Read full file
{ "filePath": "src/app.ts" }

// Read lines 50-100
{ "filePath": "src/app.ts", "startLine": 50, "endLine": 100 }

// Read 30 lines from line 200
{ "filePath": "src/app.ts", "offset": 200, "limit": 30 }

// List directory
{ "filePath": "src/" }
```

Returns line-numbered content (e.g. `1: const x = 1`). Directories return sorted entries with
trailing `/` for subdirectories. Binary files return a size-only message. Image and PDF files
return metadata suitable for UI preview. Output is capped at 50KB.

For symbol inspection with call-graph annotations, use `aft_zoom`.

---

### write

Write the full content of a file. Creates the file (and any missing parent directories) if it
doesn't exist. Backs up any existing content before overwriting.

```json
{ "filePath": "src/config.ts", "content": "export const TIMEOUT = 10000;\n" }
```

Returns inline LSP diagnostics if type errors are introduced. Auto-formats using the project's
configured formatter (biome, prettier, etc.).

For partial edits (find/replace), use `edit` instead.

---

### edit

The main editing tool. Mode is determined by which parameters you pass:

**Find and replace** — pass `filePath` + `oldString` + `newString`:

```json
{ "filePath": "src/config.ts", "oldString": "const TIMEOUT = 5000", "newString": "const TIMEOUT = 10000" }
```

Matching uses a 4-pass fuzzy fallback: exact match first, then trailing-whitespace trim, then
both-ends trim, then Unicode normalization. Returns an error if multiple matches exist — use
`occurrence: N` (0-indexed) to pick one, or `replaceAll: true` to replace all.

**Symbol replace** — pass `filePath` + `symbol` + `content`:

```json
{
  "filePath": "src/utils.ts",
  "symbol": "formatDate",
  "content": "export function formatDate(d: Date): string {\n  return d.toISOString().split('T')[0];\n}"
}
```

Includes decorators, doc comments, and attributes in the replacement range.

**Batch edits** — pass `filePath` + `edits` array. Atomic: all edits apply or none do.

```json
{
  "filePath": "src/constants.ts",
  "edits": [
    { "oldString": "VERSION = '1.0'", "newString": "VERSION = '2.0'" },
    { "startLine": 5, "endLine": 7, "content": "// updated header\n" }
  ]
}
```

Set `content` to `""` to delete lines. Per-edit `occurrence` is supported.

**Multi-file transaction** — pass `operations` array. Rolls back all files if any operation fails.

```json
{
  "operations": [
    { "file": "a.ts", "command": "write", "content": "..." },
    { "file": "b.ts", "command": "edit_match", "match": "x", "replacement": "y" }
  ]
}
```

**Glob replace** — use a glob as `filePath` with `replaceAll: true`:

```json
{ "filePath": "src/**/*.ts", "oldString": "oldName", "newString": "newName", "replaceAll": true }
```

All modes support `dryRun: true` to preview as a diff without modifying files. LSP diagnostics
are returned automatically after every edit (unless `dryRun` is set) — if type errors are
introduced, they appear inline in the response.

---

### apply_patch

Apply a multi-file patch using the `*** Begin Patch` format. Creates, updates, deletes, and
renames files atomically — if any operation fails, all revert.

```
*** Begin Patch
*** Add File: path/to/new-file.ts
+line 1
+line 2
*** Update File: path/to/existing-file.ts
@@ context anchor line
-old line
+new line
*** Delete File: path/to/obsolete-file.ts
*** End Patch
```

Context anchors (`@@`) use fuzzy matching to handle whitespace and Unicode differences.
Returns LSP diagnostics inline for any updated files that introduce type errors.

---

### ast_grep_search

Search for structural code patterns using meta-variables. Patterns must be complete AST nodes.

```json
{ "pattern": "console.log($MSG)", "lang": "typescript" }
```

- `$VAR` matches a single AST node
- `$$$` matches multiple nodes (variadic)

Returns matches with file, line (1-based), column, matched text, and captured variable values.
Add `contextLines: 3` to include surrounding lines.

```json
// Find all async functions in JS/TS
{ "pattern": "async function $NAME($$$) { $$$ }", "lang": "typescript" }
```

---

### ast_grep_replace

Replace structural code patterns across files. Applies changes by default — set `dryRun: true` to preview.

```json
{ "pattern": "console.log($MSG)", "rewrite": "logger.info($MSG)", "lang": "typescript" }
```

Meta-variables captured in `pattern` are available in `rewrite`. Returns unified diffs per file
in dry-run mode, or writes changes with backups when applied.

---

### lsp_diagnostics

Get errors, warnings, and hints from the language server. Lazily spawns the appropriate server
(typescript-language-server, pyright, rust-analyzer, gopls) on first use.

```json
// Check a single file
{ "filePath": "src/api.ts", "severity": "error" }

// Check all files in a directory
{ "directory": "src/", "severity": "all" }

// Wait for fresh diagnostics after an edit
{ "filePath": "src/api.ts", "waitMs": 2000 }
```

Returns `{ file, line, column, severity, message, code }` per diagnostic.

---

### aft_outline

Returns all top-level symbols in a file with their kind, name, line range, visibility, and nested
`members` (methods in classes, sub-headings in Markdown). Accepts a single `filePath`, a `files`
array, or a `directory` to outline all source files recursively.

For **Markdown** files (`.md`, `.mdx`): returns heading hierarchy with section ranges — each
heading becomes a symbol you can read by name.

```json
// Outline two files at once
{ "files": ["src/server.ts", "src/router.ts"] }

// Outline all source files in a directory
{ "directory": "src/auth" }
```

---

### aft_zoom

Inspect code symbols with call-graph annotations. Returns the full source of named symbols with
`calls_out` (what it calls) and `called_by` (what calls it) annotations.

Use this when you need to understand a specific function, class, or type in detail — not for
reading entire files (use `read` for that).

```json
// Inspect a single symbol
{ "filePath": "src/app.ts", "symbol": "handleRequest" }

// Inspect multiple symbols in one call
{ "filePath": "src/app.ts", "symbols": ["Config", "createApp"] }
```

For Markdown files, use the heading text as the symbol name (e.g. `"symbol": "Architecture"`).

---

### aft_delete

Delete a file with an in-memory backup. The backup survives for the session and can be restored
via `aft_safety`.

```json
{ "filePath": "src/deprecated/old-utils.ts" }
```

Returns `{ file, deleted, backup_id }` on success.

---

### aft_move

Move or rename a file. Creates parent directories for the destination automatically. Falls back
to copy+delete for cross-filesystem moves. Backs up the original before moving.

```json
{ "filePath": "src/helpers.ts", "destination": "src/utils/helpers.ts" }
```

Returns `{ file, destination, moved, backup_id }` on success.

---

### aft_navigate

Call graph and data-flow analysis across the workspace.

| Mode | What it does |
|------|-------------|
| `call_tree` | What does this function call? (forward, default depth 5) |
| `callers` | Where is this function called from? (reverse, default depth 1) |
| `trace_to` | How does execution reach this function from entry points? |
| `impact` | What callers are affected if this function changes? |
| `trace_data` | Follow a value through assignments and parameters. Needs `expression`. |

```json
// Find everything that would break if processPayment changes
{
  "op": "impact",
  "filePath": "src/payments/processor.ts",
  "symbol": "processPayment",
  "depth": 3
}
```

---

### aft_import

Language-aware import management for TS, JS, TSX, Python, Rust, and Go.

```json
// Add named imports with auto-grouping and deduplication
{
  "op": "add",
  "filePath": "src/api.ts",
  "module": "react",
  "names": ["useState", "useEffect"]
}

// Remove a single named import
{ "op": "remove", "filePath": "src/api.ts", "module": "react", "removeName": "useEffect" }

// Re-sort and deduplicate all imports by language convention
{ "op": "organize", "filePath": "src/api.ts" }
```

---

### aft_transform

Scope-aware structural transformations that handle indentation correctly.

| Op | Description |
|----|-------------|
| `add_member` | Insert a method or field into a class, struct, or impl block |
| `add_derive` | Add Rust derive macros (deduplicates) |
| `wrap_try_catch` | Wrap a TS/JS function body in try/catch |
| `add_decorator` | Add a Python decorator to a function or class |
| `add_struct_tags` | Add or update Go struct field tags |

```json
// Add a method to a TypeScript class
{
  "op": "add_member",
  "filePath": "src/user.ts",
  "container": "UserService",
  "code": "async deleteUser(id: string): Promise<void> {\n  await this.db.users.delete(id);\n}",
  "position": "last"
}
```

All ops support `dryRun` and `validate` (`"syntax"` or `"full"`).

---

### aft_refactor

Workspace-wide refactoring that updates imports and references across all files.

| Op | Description |
|----|-------------|
| `move` | Move a symbol to another file, updating all imports workspace-wide |
| `extract` | Extract a line range (1-based) into a new function (auto-detects parameters) |
| `inline` | Replace a call site (1-based `callSiteLine`) with the function's body |

```json
// Move a utility function to a shared module
{
  "op": "move",
  "filePath": "src/pages/home.ts",
  "symbol": "formatCurrency",
  "destination": "src/utils/format.ts"
}
```

`move` saves a checkpoint before mutating anything. Use `dryRun: true` to preview as a diff.

---

### aft_safety

Backup and recovery for risky edits.

| Op | Description |
|----|-------------|
| `undo` | Undo the last edit to a file |
| `history` | List all edit snapshots for a file |
| `checkpoint` | Save a named snapshot of tracked files |
| `restore` | Restore files to a named checkpoint |
| `list` | List all available checkpoints |

```json
// Checkpoint before a multi-file refactor
{ "op": "checkpoint", "name": "before-auth-refactor" }

// Restore if something goes wrong
{ "op": "restore", "name": "before-auth-refactor" }
```

> **Note:** Backups are held in-memory for the session lifetime (lost on restart). Per-file undo
> stack is capped at 20 entries — oldest snapshots are evicted when exceeded.

---

## Configuration

AFT uses a two-level config system: user-level defaults plus project-level overrides.
Both files are JSONC (comments allowed).

**User config** — applies to all projects:
```
~/.config/opencode/aft.jsonc
```

**Project config** — overrides user config for a specific project:
```
.opencode/aft.jsonc
```

### Config Options

```jsonc
{
  // Replace opencode's built-in read/write/edit/apply_patch and
  // ast_grep_search/ast_grep_replace/lsp_diagnostics with AFT-enhanced versions.
  // Default: true. Set to false to use aft_ prefix on all tools instead.
  "hoist_builtin_tools": true,

  // Auto-format files after every edit. Default: true
  "format_on_edit": true,

  // Auto-validate after edits: "syntax" (tree-sitter, fast) or "full" (runs type checker)
  "validate_on_edit": "syntax",

  // Per-language formatter overrides (auto-detected from project config files if omitted)
  // Keys: "typescript", "python", "rust", "go"
  // Values: "biome" | "prettier" | "deno" | "ruff" | "black" | "rustfmt" | "goimports" | "gofmt" | "none"
  "formatter": {
    "typescript": "biome",
    "rust": "rustfmt"
  },

  // Per-language type checker overrides (auto-detected if omitted)
  // Keys: "typescript", "python", "rust", "go"
  // Values: "tsc" | "biome" | "pyright" | "ruff" | "cargo" | "go" | "staticcheck" | "none"
  "checker": {
    "typescript": "biome"
  },

  // Tool surface level: "minimal" | "recommended" (default) | "all"
  // minimal:     aft_outline, aft_zoom, aft_safety only (no hoisting)
  // recommended: minimal + hoisted tools + lsp_diagnostics + ast_grep + aft_import
  // all:         recommended + aft_navigate, aft_delete, aft_move, aft_transform, aft_refactor
  "tool_surface": "recommended",

  // List of tool names to disable after surface filtering
  "disabled_tools": []
}
```

AFT auto-detects the formatter and checker from project config files (`biome.json` → biome,
`.prettierrc` → prettier, `Cargo.toml` → rustfmt, `pyproject.toml` → ruff/black, `go.mod` →
goimports). Local tool binaries (biome, prettier, tsc, pyright) are discovered in
`node_modules/.bin` before falling back to the system PATH. You only need per-language overrides
if auto-detection picks the wrong tool or you want to pin a specific formatter.

---

## Architecture

AFT is two components that talk over JSON-over-stdio:

```
OpenCode agent
     |
     | tool calls
     v
@cortexkit/aft-opencode (TypeScript plugin)
  - Hoists enhanced read/write/edit/apply_patch/ast_grep_*/lsp_diagnostics
  - Registers aft_outline/navigate/import/transform/refactor/safety/delete/move
  - Manages a BridgePool (one aft process per project directory)
  - Resolves the binary path (cache → npm → PATH → cargo → download)
     |
     | JSON-over-stdio (newline-delimited)
     v
aft binary (Rust)
  - tree-sitter parsing (6 language grammars)
  - Symbol resolution, call graph, diff generation
  - Format-on-edit (shells out to biome / rustfmt / etc.)
  - Backup/checkpoint management
  - ~7 MB, zero runtime dependencies
```

The binary speaks a simple request/response protocol: the plugin writes a JSON object to stdin,
the binary writes a JSON object to stdout. One process per working directory stays alive for the
session — warm parse trees, no re-spawn overhead per call.

---

## Supported Languages

| Language | Outline | Edit | Imports | Refactor |
|----------|---------|------|---------|---------|
| TypeScript | ✓ | ✓ | ✓ | ✓ |
| JavaScript | ✓ | ✓ | ✓ | ✓ |
| TSX | ✓ | ✓ | ✓ | ✓ |
| Python | ✓ | ✓ | ✓ | ✓ |
| Rust | ✓ | ✓ | ✓ | partial |
| Go | ✓ | ✓ | ✓ | partial |
| Markdown | ✓ | ✓ | — | — |

---

## Development

AFT is a monorepo: bun workspaces for TypeScript, cargo workspace for Rust.

**Requirements:** Bun ≥ 1.0, Rust stable toolchain (1.80+).

```sh
# Install JS dependencies
bun install

# Build the Rust binary
cargo build --release

# Build the TypeScript plugin
bun run build

# Run all tests
bun run test        # TypeScript tests
cargo test          # Rust tests

# Lint and format
bun run lint        # biome check
bun run lint:fix    # biome check --write
bun run format      # biome format + cargo fmt
```

**Project layout:**

```
opencode-aft/
├── crates/
│   └── aft/              # Rust binary (tree-sitter core)
│       └── src/
├── packages/
│   ├── opencode-plugin/  # TypeScript OpenCode plugin (@cortexkit/aft-opencode)
│   │   └── src/
│   │       ├── tools/    # One file per tool group
│   │       ├── config.ts # Config loading and schema
│   │       └── downloader.ts
│   └── npm/              # Platform-specific binary packages
└── scripts/
    └── version-sync.mjs  # Keeps npm and cargo versions in sync
```

---

## Roadmap

- C/C++ language support
- LSP integration for type-aware symbol resolution (partially implemented)
- Streaming responses for large call trees
- Watch mode for live outline updates

---

## Contributing

Bug reports and pull requests are welcome. For larger changes, open an issue first to discuss
the approach.

The binary protocol is documented in `crates/aft/src/main.rs`. Adding a new command means
implementing it in Rust and adding a corresponding tool definition (or extending an existing one)
in `packages/opencode-plugin/src/tools/`.

Run `bun run format` and `cargo fmt` before submitting. The CI will reject unformatted code.

---

## License

[MIT](LICENSE)
