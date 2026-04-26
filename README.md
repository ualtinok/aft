<p align="center">
  <img src="assets/banner.jpeg" alt="AFT — Agent File Toolkit" width="50%" />
</p>

<h1 align="center">AFT — Agent File Toolkit</h1>

<p align="center">
  <strong>Tree-sitter powered code manipulation and analysis tools for AI coding agents.</strong><br>
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
  <a href="#search-benchmarks">Benchmarks</a> ·
  <a href="#features">Features</a> ·
  <a href="#tool-reference">Tool Reference</a> ·
  <a href="#configuration">Configuration</a> ·
  <a href="#architecture">Architecture</a>
</p>

---

## Get Started

Run the unified AFT setup wizard — it auto-detects which harnesses (OpenCode, Pi) you have installed and configures each one:

```bash
bunx --bun @cortexkit/aft setup
```

That's it. On the next session start the `aft` binary downloads if needed and all tools become available. Use `--harness opencode` or `--harness pi` to target a specific harness.

### What AFT does to each harness

- **OpenCode** — replaces the built-in `read`, `write`, `edit`, `apply_patch`, `ast_grep_search`, `ast_grep_replace`, and `lsp_diagnostics` with AFT-powered versions and adds the `aft_` family on top.
- **Pi** — replaces the built-in `read`, `write`, `edit`, and `grep` and adds the `aft_` family on top.

<details>
<summary>Manual install — OpenCode</summary>

```bash
opencode plugin --global @cortexkit/aft-opencode@latest
```

or

```json
// ~/.config/opencode/config.json
{
  "plugin": ["@cortexkit/aft-opencode@latest"]
}
```
</details>

<details>
<summary>Manual install — Pi</summary>

```bash
pi install npm:@cortexkit/aft-pi
```

See the [pi-plugin README](packages/pi-plugin/README.md) for configuration details.
</details>

### CLI Commands

The unified `@cortexkit/aft` CLI works across every supported harness:

| Command | What it does |
|---|---|
| `bunx --bun @cortexkit/aft setup` | Interactive first-time setup — auto-detects installed harnesses and registers AFT with each |
| `bunx --bun @cortexkit/aft doctor` | Check configuration and auto-fix common issues across all detected harnesses |
| `bunx --bun @cortexkit/aft doctor --force` | Force-clear the OpenCode plugin cache (fixes stale `@latest` resolution) |
| `bunx --bun @cortexkit/aft doctor --issue` | Collect diagnostics and open a GitHub issue with sanitized logs |

Add `--harness opencode` or `--harness pi` to any command to target one harness explicitly.

**`setup`** — Registers AFT with each installed harness (edits `opencode.jsonc`'s `plugin` array for OpenCode, runs `pi install npm:@cortexkit/aft-pi` for Pi). When multiple harnesses are detected, prompts you to pick which ones to configure.

**`doctor`** — Checks everything that can go wrong per harness: host install, plugin registration, plugin cache version, binary cache, config parse errors, ONNX Runtime availability (for semantic search), storage directory sizes, log file status. Auto-fixes missing plugin entries and outdated caches.

**`doctor --force`** — Same as `doctor` but always clears the OpenCode plugin cache, forcing a fresh download. Use when you're on an old version and `@latest` doesn't seem to update (OpenCode caches npm packages aggressively).

**`doctor --issue`** — Collects a full diagnostic report, sanitizes your username and home path out of the logs, and files a GitHub issue. If you have `gh` installed, it submits directly; otherwise it writes the report to `./aft-issue-<timestamp>.md` and opens the new-issue page in your browser.

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
for backups, formatting, inline diagnostics, and symbol-aware operations. With the experimental
search index enabled, `grep` and `glob` are also hoisted with a trigram index for sub-millisecond
search on any project size.

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

```
src/auth/session.ts
  E fn    createSession(userId: string, opts?: SessionOpts): Promise<Session> 12:38
  E fn    validateToken(token: string): boolean 40:52
  E fn    refreshSession(sessionId: string): Promise<Session> 54:71
  - fn    signPayload(data: Record<string, unknown>): string 73:80
  E type  SessionOpts 5:10
  E var   SESSION_TTL 3:3
```

**2. Zoom into the specific function:**

```json
// aft_zoom
{ "filePath": "src/auth/session.ts", "symbol": "validateToken" }
```

```
src/auth/session.ts:40-52
  calls_out: verifyJwt (src/auth/jwt.ts:8), isExpired (src/auth/utils.ts:15)
  called_by: authMiddleware (src/middleware/auth.ts:22), handleLogin (src/routes/login.ts:45)

  37: // --- context_before ---
  38:
  39: /** Validate a JWT token and check expiration. */
  40: export function validateToken(token: string): boolean {
  41:   if (!token) return false;
  42:   const decoded = verifyJwt(token);
  43:   if (!decoded) return false;
  44:   return !isExpired(decoded.exp);
  45: }
  46:
  47: // --- context_after ---
  48: export function refreshSession(sessionId: string): Promise<Session> {
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

## Search Benchmarks

With `experimental_search_index: true`, AFT builds a trigram index in the background and serves
grep queries from memory. Here's how it compares to ripgrep on real codebases:

### opencode-aft (253 files)

| Query | ripgrep | AFT | Speedup |
|-------|---------|-----|---------|
| `validate_path` | 31.4ms | 1.48ms | **21x** |
| `BinaryBridge` | 31.0ms | 1.3ms | **24x** |
| `fn handle_grep` | 31.3ms | 0.2ms | **136x** |
| `experimental_search_index` | 31.5ms | 0.4ms | **71x** |

### reth (1,878 Rust files)

| Query | ripgrep | AFT | Speedup |
|-------|---------|-----|---------|
| `impl Display for` | 98.9ms | 1.10ms | **90x** |
| `BlockNumber` | 61.6ms | 2.19ms | **28x** |
| `EthApiError` | 32.7ms | 1.31ms | **25x** |
| `fn execute` | 36.6ms | 2.19ms | **17x** |

### Chromium/base (3,953 C++ files)

| Query | ripgrep | AFT | Speedup |
|-------|---------|-----|---------|
| `WebContents` | 69.5ms | 0.29ms | **236x** |
| `StringPiece` | 51.8ms | 0.78ms | **66x** |
| `NOTREACHED` | 51.6ms | 2.16ms | **24x** |
| `base::Value` | 54.4ms | 1.13ms | **48x** |

Rare queries see the biggest gains — the trigram index narrows candidates to a few files instantly.
High-match queries still benefit from `memchr` SIMD scanning and early termination.

Index builds in ~2s for most projects (under 2K files). Larger codebases like Chromium/base
(~4K files) take ~2 minutes for the initial build. Once built, the index persists to disk for
instant cold starts and stays fresh via file watcher and mtime verification.

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
- **Git conflict viewer** — show all merge conflicts across the repository in a single call with line-numbered regions
- **Indexed search** *(experimental)* — trigram-indexed `grep` and `glob` that hoist opencode's built-ins, with background index building, disk persistence, and compressed output mode
- **Semantic search** *(experimental)* — search code by meaning using local embeddings (fastembed + all-MiniLM-L6-v2), with cAST-style symbol chunking, cosine similarity ranking, and disk persistence
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
| `grep` | opencode grep *(experimental)* | Trigram-indexed regex search with compressed output | `pattern`, `path`, `include`, `exclude` |
| `glob` | opencode glob *(experimental)* | Indexed file discovery with compressed output | `pattern`, `path` |

### AFT-only tools

Always registered with `aft_` prefix regardless of hoisting setting.

**Recommended tier** (default):

| Tool | Description | Key Params |
|------|-------------|------------|
| `aft_outline` | Structural outline of a file, files, or directory | `filePath`, `files[]`, `directory` |
| `aft_zoom` | Inspect symbols with call-graph annotations | `filePath`, `symbol`, `symbols[]` |
| `aft_import` | Language-aware import add/remove/organize | `op`, `filePath`, `module`, `names[]` |
| `aft_conflicts` | Show all git merge conflicts with line-numbered regions | *(none)* |
| `aft_search` | Semantic code search by meaning *(experimental)* | `query`, `topK` |
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

On-demand LSP file/scope check. Lazily spawns the relevant language server, opens the document, prefers
LSP 3.17 pull diagnostics where supported (rust-analyzer, gopls, ty), and falls back to push + waitMs
for servers that don't support pull (bash-language-server, yaml-language-server, typescript-language-server).

**Not** a project-wide type checker — for full coverage run `tsc --noEmit`, `cargo check`,
`pyright src/`, etc. AFT's LSP is for fast feedback during edits.

**Built-in servers (6 + 1 experimental):** TypeScript (`.ts`/`.tsx`/`.js`/`.jsx`), Pyright (Python),
rust-analyzer (Rust), gopls (Go), bash-language-server (`.sh`/`.bash`/`.zsh`),
yaml-language-server (`.yaml`/`.yml`), and ty (Python, gated by `experimental_lsp_ty`).

User-defined servers go in `lsp.servers` (see Configuration). Disable any built-in via `lsp.disabled`.

```json
// Check a single file (pull where supported, push fallback otherwise)
{ "filePath": "src/api.ts", "severity": "error" }

// Check files under a directory (workspace pull from active servers + 200-file walk for unchecked listing)
{ "directory": "src/", "severity": "all" }

// Wait up to 2s for push diagnostics on push-only servers (bash, yaml, typescript)
{ "filePath": "deploy.sh", "waitMs": 2000 }
```

Response shape:

```jsonc
{
  "diagnostics": [{ "file", "line", "column", "end_line", "end_column", "severity", "message", "code" }],
  "total": 2,
  "files_with_errors": 1,
  "complete": true,                 // true = trustable absence of diagnostics; false = partial result
  "lsp_servers_used": [             // per-server status; empty array means nothing was checked
    { "id": "rust-analyzer", "status": "pull_ok" },
    { "id": "bash-language-server", "status": "binary_not_installed" }
  ],
  "unchecked_files": []              // directory mode only — files we couldn't get info for
}
```

**Reading honestly:** `total: 0` with empty `lsp_servers_used` means **nothing was checked** —
install the relevant LSP server (see warnings on plugin startup). `total: 0` with `pull_ok` /
`push_only` means the file is genuinely clean.

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

### aft_conflicts

Show all git merge conflicts across the repository in a single call. Auto-discovers conflicted
files via `git ls-files --unmerged`, parses conflict markers, and returns line-numbered regions
with 3 lines of surrounding context — the same format as `read` output.

```json
{}
```

No parameters required. Returns output like:

```
9 files, 13 conflicts

── src/manager.ts [3 conflicts] ──

  15:   resolveInheritedPromptTools,
  16:   createInternalAgentTextPart,
  17: } from "../../shared"
  18: <<<<<<< HEAD
  19: import { normalizeAgentForPrompt } from "../../shared/agent-display-names"
  20: =======
  21: import { applySessionPromptParams } from "../../shared/session-prompt-params-helpers"
  22: >>>>>>> upstream/dev
  23: import { setSessionTools } from "../../shared/session-tools-store"
```

Use `edit` with the full conflict block (including markers) as `oldString` to resolve each conflict.

When a `git merge` or `git rebase` produces conflicts, the plugin automatically appends a hint
suggesting `aft_conflicts` to the bash output.

---

### grep *(experimental)*

Trigram-indexed regex search that hoists opencode's built-in `grep`. Requires
`experimental_search_index: true` in config. The trigram index is built in a background thread
at session start, persisted to disk for fast cold starts, and kept fresh via file watcher.
Falls back to direct file scanning when the index isn't ready.

For out-of-project paths, shells out to ripgrep matching opencode's exact flags.

```json
{ "pattern": "handleRequest", "include": "*.ts" }
```

Returns matches grouped by file with relative paths, sorted by modification time (newest first),
capped at 100 matches:

```
src/server.ts
42: export async function handleRequest(req: Request) {
89:     return handleRequest(retryReq)

src/test/server.test.ts
15: import { handleRequest } from "../server"

Found 3 match(es) across 2 file(s). [index: ready]
```

Files with more than 5 matches show the first 5 and `... and N more matches`. Lines are truncated
at 200 characters.

Parameters: `pattern` (required), `path` (optional — scope to subdirectory or absolute path),
`include` (glob filter, e.g. `"*.ts"`), `exclude` (negate glob), `case_sensitive` (default true).

---

### glob *(experimental)*

Indexed file discovery that hoists opencode's built-in `glob`. Requires
`experimental_search_index: true`. Returns absolute paths sorted by modification time,
capped at 100 files.

```json
{ "pattern": "**/*.test.ts" }
```

Returns relative paths. For small result sets, a flat list:

```
3 files matching **/*.test.ts

src/server.test.ts
src/utils.test.ts
src/auth/login.test.ts
```

For larger result sets (>20 files), groups by directory:

```
20 files matching **/*.test.ts

src/ (8 files)
  server.test.ts, utils.test.ts, config.test.ts, ...

src/auth/ (4 files)
  login.test.ts, session.test.ts, token.test.ts, permissions.test.ts

... and 8 more files in 3 directories
```

Parameters: `pattern` (required), `path` (optional — scope to subdirectory or absolute path).

---

### aft_search *(experimental)*

Find symbols by **concept** when grep keywords fall short. Returns ranked code matches with
similarity scores. Requires `experimental_semantic_search: true` and
[ONNX Runtime](https://onnxruntime.ai/) installed on the system.

**When to use it:**
- Exploring an unfamiliar area: *"where is rate limiting handled"*
- Concept doesn't appear as a literal string: *"retry logic"*, *"cache invalidation"*
- After grep attempts came back empty or noisy
- You know roughly what the function does but not its name

**When NOT to use it:**
- Specific symbol name → use grep
- Error message or stack trace → use grep
- File/module structure → use `aft_outline`
- Following a call chain → use `aft_navigate`

Uses a local embedding model (all-MiniLM-L6-v2, ~22MB, downloaded on first use) to embed
**code symbols only** (functions, classes, methods, structs, etc.) and matches queries by
cosine similarity. Markdown headings are **not** indexed — they tend to dominate result
lists and crowd out actual code matches; use grep for prose. No API keys or external
services needed.

**Install ONNX Runtime:**
- **macOS:** `brew install onnxruntime`
- **Linux (Debian/Ubuntu):** `apt install libonnxruntime`
- **Linux (other):** Download from [ONNX Runtime releases](https://github.com/microsoft/onnxruntime/releases)
- **Windows:** `winget install Microsoft.ONNXRuntime`

Without ONNX Runtime, all other AFT tools work normally — only `aft_search` is unavailable.

```json
{ "query": "authentication middleware that validates JWT tokens" }
```

Returns ranked results with relevance scores and code snippets:

```
crates/aft/src/commands/configure.rs
  handle_configure (function, exported) 17:253 [0.42]
    pub fn handle_configure(req: &RawRequest, ctx: &AppContext) -> Response {
      let root = match req.params.get("project_root")...
      ...

packages/opencode-plugin/src/bridge.ts
  checkVersion (function) 150:175 [0.38]
    private async checkVersion(): Promise<void> {
      ...

Found 10 results [semantic index: ready]
```

The index is built in a background thread at session start, persisted to disk for fast cold
start, and uses cAST-style enrichment (file path + kind + name + signature + body snippet)
for better embedding quality.

Parameters: `query` (required — natural language description), `topK` (optional — default 10).

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
  // recommended: minimal + hoisted tools + lsp_diagnostics + ast_grep + aft_import + aft_conflicts
  //              + grep/glob (when experimental_search_index is enabled)
  //              + aft_search (when experimental_semantic_search is enabled)
  // all:         recommended + aft_navigate, aft_delete, aft_move, aft_transform, aft_refactor
  "tool_surface": "recommended",

  // List of tool names to disable after surface filtering
  "disabled_tools": [],

  // --- Experimental ---

  // Enable trigram-indexed grep/glob that hoist opencode's built-ins.
  // Builds a background index on session start, persists to disk, updates via file watcher.
  // Falls back to direct scanning when the index isn't ready or for out-of-project paths.
  // Default: false
  "experimental_search_index": false,

  // Enable semantic code search (aft_search tool).
  // Requires ONNX Runtime installed (brew install onnxruntime on macOS).
  // Builds embeddings for all symbols using a local model (all-MiniLM-L6-v2, ~22MB).
  // The model is downloaded on first use. Index persists to disk for fast cold start.
  // Default: false
  "experimental_semantic_search": false,

  // Restrict all file operations to the project root directory.
  // Default: false (matches opencode's permission-based model — operations prompt via ctx.ask)
  "restrict_to_project_root": false,

  // Maximum source files allowed for call-graph operations (callers, trace_to,
  // trace_data, impact). Projects above this size return "project_too_large"
  // with guidance to open a specific subdirectory. Does not affect grep,
  // glob, read, edit, or any other tool.
  // Default: 20000 (covers typical monorepos; rejects OS-wide roots like ~/Work).
  "max_callgraph_files": 20000,

  // Language servers used for post-edit diagnostics.
  //
  // Built-in servers (auto-registered when their binary is on PATH):
  //   typescript-language-server, pyright-langserver, rust-analyzer, gopls,
  //   bash-language-server, yaml-language-server
  //
  // Add your own with `lsp.servers`. Disable any with `lsp.disabled`.
  "lsp": {
    "servers": {
      "tinymist": {
        "extensions": [".typ"],
        "binary": "tinymist",
        "args": [],
        "root_markers": [".git", "typst.toml"],
        "env": {                  // optional — extra env vars passed to the spawned server
          "TYPST_FONT_PATHS": "/usr/share/fonts"
        },
        "initialization_options": {  // optional — server-specific LSP `initializationOptions`
          "formatterMode": "typstyle"
        }
      }
    },
    "disabled": ["pyright"],
    "python": "ty",  // "auto" (default) | "pyright" | "ty"

    // LRU cap for the in-memory diagnostic cache.
    // Bigger = more files retained across the session.
    // Default: 5000. Set to 0 to disable cap (live dangerously on huge monorepos).
    "diagnostic_cache_size": 5000
  },

  // Enable Astral's experimental ty Python type checker. Implied when lsp.python === "ty".
  // Default: false
  "experimental_lsp_ty": false
}
```

AFT auto-detects the formatter and checker from project config files (`biome.json` → biome,
`.prettierrc` → prettier, `Cargo.toml` → rustfmt, `pyproject.toml` → ruff/black, `go.mod` →
goimports). Local tool binaries (biome, prettier, tsc, pyright) are discovered in
`node_modules/.bin` before falling back to the system PATH. You only need per-language overrides
if auto-detection picks the wrong tool or you want to pin a specific formatter.

### Language servers (LSP)

AFT runs language servers in-process for post-edit diagnostics and on-demand `lsp_diagnostics`
calls. Servers are spawned lazily — only when a file matching their extensions is touched, and
only if their binary is on `PATH`.

**Built-in servers** (auto-registered, no config needed):

| Server | Languages | Binary |
|---|---|---|
| TypeScript Language Server | `.ts .tsx .js .jsx .mjs .cjs` | `typescript-language-server` |
| Pyright | `.py .pyi` | `pyright-langserver` |
| rust-analyzer | `.rs` | `rust-analyzer` |
| gopls | `.go` | `gopls` |
| bash-language-server | `.sh .bash .zsh` | `bash-language-server` |
| yaml-language-server | `.yaml .yml` | `yaml-language-server` |

**Experimental:** `ty` (Astral's Python type checker) — gated behind `experimental_lsp_ty: true`
or `lsp.python: "ty"`. When enabled, ty runs alongside Pyright unless you also disable Pyright
via `lsp.disabled: ["pyright"]` (or use `lsp.python: "ty"` which does both automatically).

**Registering a custom server:** add it under `lsp.servers` in your config. The example
configuration above shows registering `tinymist` for Typst files. Required fields per server:
`extensions` (array, leading `.` is stripped), `binary` (PATH lookup name). Optional:
`args`, `root_markers` (defaults to `[".git"]`), `disabled`.

**Disabling a built-in:** add the server's id (`"pyright"`, `"yaml-language-server"`, etc.) to
`lsp.disabled`. IDs are case-insensitive.

**Custom server fields:**

| Field | Required | Description |
|---|---|---|
| `extensions` | yes | Array of file extensions (leading `.` is stripped) |
| `binary` | yes | Binary name resolved against `PATH` |
| `args` | no | Args passed to the server (default: `[]`) |
| `root_markers` | no | Filenames whose presence anchors the workspace root (default: `[".git"]`) |
| `env` | no | Extra environment variables for the spawned process |
| `initialization_options` | no | Passed to the server's LSP `initialize` request |
| `disabled` | no | Skip this server even though it's registered |

**Missing-tool warnings:** on startup, AFT detects configured-but-missing formatters, type
checkers, and LSP binaries (for languages your project actually uses) and surfaces a one-time
notification per warning via OpenCode's ignored-message channel. Dismissed warnings do not
re-fire on plugin updates — dedupe is per-warning-content, persisted in
`<storage_dir>/warned_tools.json`.

### Working with large repositories

If you point AFT at a very large directory (monorepo root, `~/Work`, `/home`, etc.), certain
features guard against unbounded work to keep the bridge responsive:

- **Call-graph ops** (`callers`, `trace_to`, `trace_data`, `impact`) return `project_too_large`
  above `max_callgraph_files` (default 20,000). The plugin logs a warning at startup when this
  threshold is exceeded so you know before making a tool call.
- **Semantic indexing** is skipped above 10,000 source files.
- **`grep`, `glob`, `read`, `edit`, and other tools** work at any size.

Commands with heavier workloads get longer per-call timeouts: 60s for `callers`, `trace_to`,
`trace_data`, `impact`, `grep`, `glob`; 45s for `semantic_search`; 30s for everything else.
For best results in very large trees, point AFT at a specific project subdirectory.

---

## Architecture

AFT is two components that talk over JSON-over-stdio:

```
OpenCode agent
     |
     | tool calls
     v
@cortexkit/aft-opencode (TypeScript plugin)
  - Hoists enhanced read/write/edit/apply_patch/ast_grep_*/lsp_diagnostics/grep/glob
  - Registers aft_outline/navigate/import/transform/refactor/safety/delete/move/search
  - Manages a BridgePool (one aft process per session)
  - Resolves the binary path (cache → npm → PATH → cargo → download)
     |
     | JSON-over-stdio (newline-delimited)
     v
aft binary (Rust)
  - tree-sitter parsing (14 language grammars)
  - Symbol resolution, call graph, diff generation
  - Format-on-edit (shells out to biome / rustfmt / etc.)
  - Backup/checkpoint management
  - Trigram search index (experimental: background thread, disk persistence, file watcher)
  - Semantic search with local embeddings (experimental: fastembed + all-MiniLM-L6-v2)
  - Persistent storage at ~/.local/share/opencode/storage/plugin/aft/
```

The binary speaks a simple request/response protocol: the plugin writes a JSON object to stdin,
the binary writes a JSON object to stdout. One process per session stays alive for the session
lifetime — warm parse trees, isolated undo history, no re-spawn overhead per call.

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
| C | ✓ | ✓ | — | — |
| C++ | ✓ | ✓ | — | — |
| C# | ✓ | ✓ | — | — |
| Zig | ✓ | ✓ | — | — |
| Bash | ✓ | ✓ | — | — |
| HTML | ✓ | ✓ | — | — |
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

- MCP server for Claude Code, Cursor, and other MCP-compatible hosts
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
