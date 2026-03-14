# Requirements

This file is the explicit capability and coverage contract for the project.

Use it to track what is actively in scope, what has been validated by completed work, what is intentionally deferred, and what is explicitly out of scope.

## Active

### R001 — Persistent binary architecture
- Class: core-capability
- Status: validated
- Description: The aft Rust binary runs as a persistent process, receiving JSON commands on stdin and writing JSON responses to stdout. Keeps parse state, caches, and edit history in memory between calls.
- Why it matters: Per-command spawning would re-parse grammars and lose all cached state on every call, defeating incremental parsing and call graph caching.
- Source: user
- Primary owning slice: M001/S01
- Supporting slices: none
- Validation: S01 — 120 sequential commands without restart, malformed JSON recovery, clean shutdown on EOF
- Notes: Process must handle graceful shutdown, crash recovery signaling to the plugin.

### R002 — Multi-language tree-sitter parsing
- Class: core-capability
- Status: validated
- Description: Tree-sitter grammars embedded for TypeScript, JavaScript, TSX/JSX, Python, Rust, and Go. Language auto-detected from file extension. Symbol extraction queries per language for functions, classes, methods, structs, interfaces, enums.
- Why it matters: This is the foundation for every semantic operation — editing by symbol, outline, zoom, call graph all depend on accurate symbol extraction.
- Source: user
- Primary owning slice: M001/S02
- Supporting slices: none
- Validation: S02 — 53 unit tests prove symbol extraction across all 6 languages with representative code patterns. All symbol kinds, scope chains, export detection, arrow functions, impl blocks, receiver methods, decorated functions verified.
- Notes: Web-first priority — TS/JS/TSX share query patterns and ship first. Python next, then Rust and Go.

### R003 — Structural reading (outline + zoom)
- Class: primary-user-loop
- Status: validated
- Description: `outline` returns a file's structural overview — all symbols with kind, name, range, signature, export status, and member nesting. `zoom` returns a single symbol's full body with surrounding context and caller/callee annotations.
- Why it matters: Replaces reading a 500-line file to find 5 lines. Agent gets targeted information in ~50 lines instead of ~500.
- Source: user
- Primary owning slice: M001/S03
- Supporting slices: none
- Validation: S03 — 19 unit tests + 8 integration tests verify nested outline structure, all symbol kinds, call annotations, context lines, error paths, and multi-language fixtures (TS, Python, Rust).
- Notes: zoom annotations include both outbound calls (what this function calls) and inbound callers (what calls this function). Annotations are file-scoped; cross-file deferred to M003.

### R004 — Semantic editing (edit by symbol name)
- Class: core-capability
- Status: active
- Description: `edit_symbol` accepts a symbol name and operation (replace, delete, insert_before, insert_after) and applies the edit to the resolved symbol's range. Returns the new range, syntax validation result, and backup ID.
- Why it matters: Agents think "change function X" but current tools require translation to line numbers. Symbol-level addressing eliminates that translation and its error rate.
- Source: user
- Primary owning slice: M001/S05
- Supporting slices: none
- Validation: unmapped
- Notes: Must handle disambiguation when multiple symbols match (return candidates with qualified names).

### R005 — Structural editing (edit by content match)
- Class: core-capability
- Status: active
- Description: `edit_match` targets lines by their content — single line match/replace or range match (from/to markers). When match is ambiguous (appears multiple times), returns all occurrences with ±2 lines context for the agent to choose by index.
- Why it matters: Useful when the agent already knows what the code looks like from earlier reads or conversation context. Faster than symbol-level for simple constant changes.
- Source: user
- Primary owning slice: M001/S05
- Supporting slices: none
- Validation: unmapped
- Notes: Complements edit_symbol — agent picks whichever is more natural for the specific edit.

### R006 — Bulk and batch editing
- Class: core-capability
- Status: active
- Description: `write` does full file write via JSON stdin (new files or complete rewrites). `batch` applies multiple edits to one file in a single call, auto-sorted bottom-to-top to prevent line drift. Batch is atomic — all succeed or all roll back.
- Why it matters: write eliminates shell escaping issues for file content. batch prevents the line-drift problem when making multiple edits in one file.
- Source: user
- Primary owning slice: M001/S05
- Supporting slices: none
- Validation: unmapped
- Notes: All content passes through JSON — no shell argument strings.

### R007 — Per-file auto-backup and undo
- Class: failure-visibility
- Status: active
- Description: Every mutation operation automatically snapshots the affected file before modifying it. `undo` restores the previous version. `edit_history` shows the stack of edits per file with timestamps and operation descriptions.
- Why it matters: Agents make mistakes. A one-step undo per file means recovery costs ~50 tokens instead of ~1500 (re-read + re-edit).
- Source: user
- Primary owning slice: M001/S04
- Supporting slices: M001/S05
- Validation: unmapped
- Notes: Undo stack is in-memory in the persistent process, with periodic flush to .aft/ directory.

### R008 — Workspace-wide checkpoints
- Class: failure-visibility
- Status: active
- Description: `checkpoint` creates a named snapshot of all tracked files. `restore_checkpoint` rolls back to a named checkpoint. `list_checkpoints` shows available checkpoints with file counts. Auto-cleanup after 24h (configurable).
- Why it matters: Agents need "save game" before risky multi-file changes. Workspace-level rollback is the safety net for experimental refactors.
- Source: user
- Primary owning slice: M001/S04
- Supporting slices: none
- Validation: unmapped
- Notes: Stored in .aft/checkpoints/ (gitignored). Lightweight file copies, not git objects.

### R009 — OpenCode plugin bridge
- Class: integration
- Status: active
- Description: TypeScript plugin registers all AFT commands as OpenCode tools with Zod schemas. Binary bridge manages the persistent process (spawn, health check, restart on crash). Platform binary resolver finds the correct binary (npm package, PATH, cargo).
- Why it matters: Without the plugin, agents can't access AFT tools. The bridge must handle process lifecycle robustly — crashed binary should auto-restart transparently.
- Source: user
- Primary owning slice: M001/S06
- Supporting slices: none
- Validation: unmapped
- Notes: Plugin is intentionally thin — all logic lives in the Rust binary.

### R010 — Post-edit syntax validation
- Class: quality-attribute
- Status: active
- Description: Every edit response includes a `syntax_valid` field from a tree-sitter re-parse (~1ms). This is the default validation level. Catches malformed code immediately after edit.
- Why it matters: Agents often produce syntax errors. Immediate feedback prevents cascading failures from building on broken code.
- Source: user
- Primary owning slice: M001/S05
- Supporting slices: none
- Validation: unmapped
- Notes: This is only tree-sitter syntax checking. Full type-checker validation is R017 (M002).

### R011 — Symbol disambiguation
- Class: quality-attribute
- Status: active
- Description: When edit_symbol or zoom targets a symbol name that matches multiple symbols (e.g., two `validate` functions in different scopes), the response returns an `ambiguous_symbol` error with a candidates list showing qualified names, files, and line numbers. Agent reissues with the qualified name.
- Why it matters: Ambiguous symbols are a common source of wrong-target edits. Explicit disambiguation prevents silent wrong-function edits.
- Source: user
- Primary owning slice: M001/S05
- Supporting slices: M001/S03
- Validation: unmapped
- Notes: Qualified names use scope chain: `ClassName.methodName`, `module::function`.

### R012 — Binary distribution
- Class: launchability
- Status: active
- Description: npm optional dependency packages per platform (darwin-arm64, darwin-x64, linux-arm64, linux-x64, win32-x64) following the esbuild/turbo pattern. CI pipeline cross-compiles for all 5 platforms. Fallback to `cargo install aft`.
- Why it matters: Users must be able to install AFT with a single `npm install` or `bun install`. Manual binary management is a non-starter for adoption.
- Source: user
- Primary owning slice: M001/S07
- Supporting slices: none
- Validation: unmapped
- Notes: @aft/core declares optionalDependencies on all platform packages. npm resolves the correct one at install time.

### R013 — Import management (6 languages)
- Class: core-capability
- Status: active
- Description: `add_import` adds imports with language-aware placement (TS named/default/type imports, Rust use tree merging, Python isort groups, Go goimports groups). `remove_import` cleans up after refactors. `organize_imports` re-sorts and re-groups all imports.
- Why it matters: Import management is high-error-rate (~15% wrong group, duplicates) and high-frequency. Language-aware automation eliminates these errors.
- Source: user
- Primary owning slice: M002/S01
- Supporting slices: none
- Validation: unmapped
- Notes: Must handle deduplication, alphabetization, and group separation per language convention.

### R014 — Scope-aware member insertion
- Class: core-capability
- Status: active
- Description: `add_member` inserts methods, fields, or functions into the correct scope of a class/struct/impl with correct indentation. Supports positioning: after/before specific member, first, or last.
- Why it matters: Manual member insertion requires calculating indentation and finding the right insertion point — error-prone for agents.
- Source: user
- Primary owning slice: M002/S02
- Supporting slices: none
- Validation: unmapped
- Notes: Rust must distinguish between `impl Struct` and `impl Trait for Struct`. Python uses indentation as scope delimiter.

### R015 — Language-specific compound operations
- Class: differentiator
- Status: active
- Description: Structural transforms that are idiomatic per language: add_derive (Rust), wrap_try_catch (TS/JS), add_decorator (Python), add_struct_tags (Go). Each modifies the AST structurally rather than string-matching.
- Why it matters: These are the operations agents do most clumsily — adding a derive to an existing attribute list, wrapping a function body in try/catch without breaking indentation.
- Source: user
- Primary owning slice: M002/S03
- Supporting slices: none
- Validation: unmapped
- Notes: Hardcoded per language for now. Extensibility is deferred (R036).

### R016 — Auto-format on save
- Class: quality-attribute
- Status: active
- Description: After every edit, detect and invoke the project's canonical formatter (prettier, rustfmt, black/ruff, gofmt) if available. Response indicates "applied" or "not_found". Agent never thinks about code style.
- Why it matters: Inconsistent formatting creates noise in diffs and wastes agent attention on style.
- Source: user
- Primary owning slice: M002/S04
- Supporting slices: none
- Validation: unmapped
- Notes: Detects formatter from project config (.prettierrc, rustfmt.toml, pyproject.toml, etc.). Falls back to defaults if no config.

### R017 — Full validation mode (opt-in type checkers)
- Class: quality-attribute
- Status: active
- Description: Opt-in `validate: "full"` mode invokes external type checkers (tsc, pyright, cargo check, go vet) after an edit. Returns type errors with line numbers and messages. Default remains syntax-only (fast).
- Why it matters: Gives agents a one-stop validation option for critical edits — e.g., after multi-file transactions where confirming type safety matters.
- Source: user
- Primary owning slice: M002/S04
- Supporting slices: none
- Validation: unmapped
- Notes: Synchronous — command blocks until type checker returns. Acceptable because it's opt-in and the agent explicitly requested it.

### R018 — Dry-run mode on all mutations
- Class: failure-visibility
- Status: active
- Description: Every mutation command accepts `dry_run: true` and returns a diff preview without applying changes. Shows lines added/removed and syntax validity of the proposed change.
- Why it matters: Lets agents preview destructive operations before committing. Essential for cautious multi-file refactors.
- Source: user
- Primary owning slice: M002/S05
- Supporting slices: none
- Validation: unmapped
- Notes: Dry-run diff format should be unified diff (parseable by agents).

### R019 — Multi-file atomic transactions
- Class: core-capability
- Status: active
- Description: `transaction` applies edits to multiple files atomically — all succeed or all roll back. Each file's result is reported individually. If any file fails validation, all changes revert.
- Why it matters: Multi-file refactors with partial failures leave the codebase in a broken state. Atomic transactions eliminate partial failure.
- Source: user
- Primary owning slice: M002/S05
- Supporting slices: none
- Validation: unmapped
- Notes: Builds on the per-file backup system from R007.

### R020 — Call graph construction (lazy, incremental, file watcher)
- Class: core-capability
- Status: active
- Description: Static call graph built lazily — first query scans outward from the target, caching results. File watcher invalidates/rebuilds graph nodes for changed files. Graph respects worktree boundaries.
- Why it matters: Eager full-workspace scan is too slow for large codebases. Lazy construction with incremental updates gives fast first results and improves over time.
- Source: user
- Primary owning slice: M003/S01
- Supporting slices: none
- Validation: unmapped
- Notes: File watcher runs in the persistent process background. Must exclude node_modules, target/, venv/, etc.

### R021 — Forward call tree
- Class: primary-user-loop
- Status: active
- Description: `call_tree` expands from an entry point showing all called functions recursively with signatures, body lines, and file locations. Depth-limited with truncation indicators.
- Why it matters: Replaces the file-by-file call tracing workflow (~5000 tokens for 4 files) with a single call (~400 tokens).
- Source: user
- Primary owning slice: M003/S02
- Supporting slices: none
- Validation: unmapped
- Notes: External calls (third-party libraries) shown as leaf nodes with package name.

### R022 — Reverse caller tree
- Class: primary-user-loop
- Status: active
- Description: `callers` shows all call sites for a function, grouped by file, with each caller expandable to its own callers up to specified depth.
- Why it matters: "What calls this function?" is a fundamental navigation question that currently requires grep + manual tracing.
- Source: user
- Primary owning slice: M003/S02
- Supporting slices: none
- Validation: unmapped
- Notes: None.

### R023 — Reverse trace to entry points
- Class: differentiator
- Status: active
- Description: `trace_to` traces backward from a target function to all entry points (route handlers, event listeners, exported functions, main). Renders top-down with data threading — shows how data transforms through the call chain.
- Why it matters: "How does execution reach this function?" — the most powerful single-call navigation primitive. Replaces 5+ file reads.
- Source: user
- Primary owning slice: M003/S03
- Supporting slices: none
- Validation: unmapped
- Notes: Depends on entry point detection heuristics (R026).

### R024 — Data flow tracking
- Class: differentiator
- Status: active
- Description: `trace_data` follows a specific expression through the call chain, showing type transformations and variable renames at each hop.
- Why it matters: Understanding how a value flows through code is essential for debugging and security analysis. Currently requires manual tracing across files.
- Source: user
- Primary owning slice: M003/S04
- Supporting slices: none
- Validation: unmapped
- Notes: Static analysis — tracks through assignments, function parameters, and return values.

### R025 — Change impact analysis
- Class: differentiator
- Status: active
- Description: `impact` analyzes what breaks if a symbol is changed (e.g., add parameter, change return type). Reports direct callers needing update, indirect callers that may need update, and type impact.
- Why it matters: Agents currently guess at impact or miss callers. Automated impact analysis prevents broken-but-not-caught changes.
- Source: user
- Primary owning slice: M003/S05
- Supporting slices: none
- Validation: unmapped
- Notes: Includes suggestions for updating call sites (e.g., "add default argument").

### R026 — Entry point detection heuristics
- Class: core-capability
- Status: active
- Description: Detect entry points heuristically: route handlers (router.get/post, @Get/@Post), event listeners (on/addEventListener), exported functions, main/init/bootstrap functions, test functions (describe/it/test).
- Why it matters: trace_to needs to know where to stop tracing backward. Without entry point detection, traces go to infinity.
- Source: user
- Primary owning slice: M003/S03
- Supporting slices: none
- Validation: unmapped
- Notes: Heuristics are language-specific — Express routes vs Flask decorators vs Axum handlers.

### R027 — Worktree-aware scoping
- Class: quality-attribute
- Status: active
- Description: All operations respect the project root boundary. Call graph, file watcher, and symbol search do not crawl into node_modules, .git, target/, venv/, or other excluded directories.
- Why it matters: Without scoping, operations on large codebases include irrelevant third-party code, producing noise and performance problems.
- Source: user
- Primary owning slice: M003/S01
- Supporting slices: M001/S01
- Validation: unmapped
- Notes: Should respect .gitignore patterns by default. Configurable exclusion list.

### R028 — Move symbol with import rewiring
- Class: core-capability
- Status: active
- Description: `move_symbol` moves a function/class/type from one file to another, updates all import statements that reference it across the workspace, and adds necessary exports.
- Why it matters: Manual symbol moves require 5-10 steps (cut, paste, update imports in every consuming file). Single-call operation eliminates the most error-prone refactor.
- Source: user
- Primary owning slice: M004/S01
- Supporting slices: none
- Validation: unmapped
- Notes: Depends on call graph (knows all consumers) and import management (knows how to update imports).

### R029 — Extract function
- Class: core-capability
- Status: active
- Description: `extract_function` takes a line range, identifies free variables that become parameters, determines the return value, extracts the code into a new function, and replaces the original range with a call.
- Why it matters: Extract is the most common refactoring operation. Automating parameter inference and return type detection eliminates the manual analysis step.
- Source: user
- Primary owning slice: M004/S02
- Supporting slices: none
- Validation: unmapped
- Notes: Agent reviews and approves the extraction before it's applied.

### R030 — Inline symbol
- Class: core-capability
- Status: active
- Description: `inline_symbol` replaces a function call with the function's body, adjusting variable names and scope as needed. Inverse of extract.
- Why it matters: Removes unnecessary indirection — one-line wrapper functions, trivial helpers that obscure the logic.
- Source: user
- Primary owning slice: M004/S02
- Supporting slices: none
- Validation: unmapped
- Notes: Must handle scope conflicts when inlining (variable name collisions).

### R031 — LSP-aware architecture (provider interface)
- Class: constraint
- Status: active
- Description: Symbol resolution has a provider interface from M001 onward. Tree-sitter is the default provider. Command JSON schema includes optional `lsp_hints` fields. When the plugin has LSP data available, it enriches commands with precise symbol locations, type info, and resolved references.
- Why it matters: Pure tree-sitter resolution is ~80% accurate. LSP data pushes it to ~99%. The architecture must be ready for this upgrade path without refactoring.
- Source: user
- Primary owning slice: M001/S01
- Supporting slices: M004/S03
- Validation: S01 — LanguageProvider trait defined, optional lsp_hints field in RawRequest protocol type. Full validation deferred to M004/S03.
- Notes: M001 ships with tree-sitter only. LSP enrichment wired in M004.

### R032 — Structured JSON I/O (no shell escaping)
- Class: constraint
- Status: validated
- Description: All communication between plugin and binary uses JSON over stdin/stdout. File content, code snippets, and edit payloads are JSON string values — never shell arguments. Eliminates all shell escaping issues.
- Why it matters: Shell escaping is a persistent source of bugs in agent tools. JSON I/O makes content handling unambiguous.
- Source: user
- Primary owning slice: M001/S01
- Supporting slices: none
- Validation: S01 — all 120 sequential commands and 8 malformed scenarios flow through JSON stdin/stdout, no shell escaping in any path
- Notes: Every request is one JSON object per line. Every response is one JSON object per line.

### R033 — LSP integration via plugin mediation
- Class: integration
- Status: active
- Description: The TypeScript plugin queries OpenCode's LSP infrastructure and passes enhanced resolution data to the Rust binary as part of command JSON `lsp_hints` fields. Binary uses LSP data when available, falls back to tree-sitter when not.
- Why it matters: Completes the accuracy story — tree-sitter handles structure, LSP provides precise type-level resolution for ambiguous cases.
- Source: user
- Primary owning slice: M004/S03
- Supporting slices: none
- Validation: unmapped
- Notes: Plugin mediates — binary never connects to language servers directly.

### R034 — Web-first language priority
- Class: constraint
- Status: active
- Description: TypeScript, JavaScript, and TSX/JSX are integrated first (shared tree-sitter query patterns). Python second. Rust and Go third. Each language is fully integrated before moving to the next.
- Why it matters: Most AI agents work in TS/JS. Shipping the most-used languages first maximizes early value.
- Source: user
- Primary owning slice: M001/S02
- Supporting slices: M002/S01
- Validation: unmapped
- Notes: TS/JS/TSX share ~80% of query patterns. Python is structurally different (indent-based scope). Rust and Go each have unique constructs (impl blocks, interface embedding).

## Deferred

### R035 — Multi-language file handling
- Class: core-capability
- Status: deferred
- Description: Handle files with mixed languages (HTML with embedded script/style, Markdown with code blocks).
- Why it matters: Real-world codebases have mixed-language files. Tree-sitter supports language injection but it adds significant complexity.
- Source: research
- Primary owning slice: none
- Supporting slices: none
- Validation: unmapped
- Notes: Deferred because single-language files cover >95% of agent workflows. Can be added in a future milestone.

### R036 — User-extensible compound operation templates
- Class: differentiator
- Status: deferred
- Description: Allow users to define custom compound operations via a template system rather than hardcoding per language.
- Why it matters: Would let the community extend AFT's structural transforms without modifying the binary.
- Source: research
- Primary owning slice: none
- Supporting slices: none
- Validation: unmapped
- Notes: Deferred until the hardcoded compound operations prove the pattern and we understand what users want to extend.

### R037 — Call graph persistence to disk
- Class: quality-attribute
- Status: deferred
- Description: Persist the call graph to disk so it survives binary restarts without full rebuild.
- Why it matters: Large codebases take time to build the call graph. Persistence avoids cold-start penalty.
- Source: research
- Primary owning slice: none
- Supporting slices: none
- Validation: unmapped
- Notes: Deferred because the lazy/incremental strategy with file watcher means the graph rebuilds quickly on demand. Persistence adds state management complexity.

## Out of Scope

### R038 — Replace LSP
- Class: anti-feature
- Status: out-of-scope
- Description: AFT provides fast, approximate analysis via tree-sitter. LSP remains the source of truth for types, diagnostics, and precise resolution.
- Why it matters: Prevents scope creep into building a language server. AFT and LSP are complementary, not competing.
- Source: user
- Primary owning slice: none
- Supporting slices: none
- Validation: n/a
- Notes: AFT can consume LSP data (R033) but does not produce it.

### R039 — Code generation / scaffolding
- Class: anti-feature
- Status: out-of-scope
- Description: AFT manipulates existing code structurally. It does not generate boilerplate, scaffolding, or templates.
- Why it matters: Prevents scope creep. Code generation is a different tool category.
- Source: user
- Primary owning slice: none
- Supporting slices: none
- Validation: n/a
- Notes: Agents handle code generation themselves — AFT helps them place and edit it.

### R040 — GUI / editor integration / watch mode
- Class: anti-feature
- Status: out-of-scope
- Description: AFT is a CLI tool and OpenCode plugin. No GUI, no editor integration, no watch mode.
- Why it matters: Keeps scope focused on the agent use case. Editor plugins are a different product.
- Source: user
- Primary owning slice: none
- Supporting slices: none
- Validation: n/a
- Notes: None.

### R041 — Override built-in Edit/Write tools
- Class: anti-feature
- Status: out-of-scope
- Description: AFT tools are registered alongside built-in Edit/Write, not as replacements. The agent uses whichever is more appropriate.
- Why it matters: Prevents breaking existing workflows. AFT is additive.
- Source: user
- Primary owning slice: none
- Supporting slices: none
- Validation: n/a
- Notes: None.

## Traceability

| ID | Class | Status | Primary owner | Supporting | Proof |
|---|---|---|---|---|---|
| R001 | core-capability | validated | M001/S01 | none | S01 |
| R002 | core-capability | validated | M001/S02 | none | S02 |
| R003 | primary-user-loop | validated | M001/S03 | none | S03 |
| R004 | core-capability | active | M001/S05 | none | unmapped |
| R005 | core-capability | active | M001/S05 | none | unmapped |
| R006 | core-capability | active | M001/S05 | none | unmapped |
| R007 | failure-visibility | active | M001/S04 | M001/S05 | unmapped |
| R008 | failure-visibility | active | M001/S04 | none | unmapped |
| R009 | integration | active | M001/S06 | none | unmapped |
| R010 | quality-attribute | active | M001/S05 | none | unmapped |
| R011 | quality-attribute | active | M001/S05 | M001/S03 | unmapped |
| R012 | launchability | active | M001/S07 | none | unmapped |
| R013 | core-capability | active | M002/S01 | none | unmapped |
| R014 | core-capability | active | M002/S02 | none | unmapped |
| R015 | differentiator | active | M002/S03 | none | unmapped |
| R016 | quality-attribute | active | M002/S04 | none | unmapped |
| R017 | quality-attribute | active | M002/S04 | none | unmapped |
| R018 | failure-visibility | active | M002/S05 | none | unmapped |
| R019 | core-capability | active | M002/S05 | none | unmapped |
| R020 | core-capability | active | M003/S01 | none | unmapped |
| R021 | primary-user-loop | active | M003/S02 | none | unmapped |
| R022 | primary-user-loop | active | M003/S02 | none | unmapped |
| R023 | differentiator | active | M003/S03 | none | unmapped |
| R024 | differentiator | active | M003/S04 | none | unmapped |
| R025 | differentiator | active | M003/S05 | none | unmapped |
| R026 | core-capability | active | M003/S03 | none | unmapped |
| R027 | quality-attribute | active | M003/S01 | M001/S01 | unmapped |
| R028 | core-capability | active | M004/S01 | none | unmapped |
| R029 | core-capability | active | M004/S02 | none | unmapped |
| R030 | core-capability | active | M004/S02 | none | unmapped |
| R031 | constraint | active | M001/S01 | M004/S03 | S01 (partial) |
| R032 | constraint | validated | M001/S01 | none | S01 |
| R033 | integration | active | M004/S03 | none | unmapped |
| R034 | constraint | active | M001/S02 | M002/S01 | unmapped |
| R035 | core-capability | deferred | none | none | unmapped |
| R036 | differentiator | deferred | none | none | unmapped |
| R037 | quality-attribute | deferred | none | none | unmapped |
| R038 | anti-feature | out-of-scope | none | none | n/a |
| R039 | anti-feature | out-of-scope | none | none | n/a |
| R040 | anti-feature | out-of-scope | none | none | n/a |
| R041 | anti-feature | out-of-scope | none | none | n/a |

## Coverage Summary

- Active requirements: 30
- Mapped to slices: 34
- Validated: 4
- Unmapped active requirements: 0
