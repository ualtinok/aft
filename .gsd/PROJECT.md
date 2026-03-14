# Project

## What This Is

Agent File Toolkit (AFT) — a Rust binary + OpenCode TypeScript plugin that gives AI coding agents semantic file manipulation and code navigation primitives. Replaces the read-grep-edit-read cycle with operations that match how agents reason about code: edit by symbol name, trace call paths in one call, manage imports with a single operation, checkpoint/restore without git overhead.

Two components: a Rust binary (`aft`) that embeds tree-sitter grammars for 6 languages and does all computation, and a thin TypeScript OpenCode plugin that bridges tool calls to the binary via JSON over stdin/stdout.

## Core Value

One-call semantic file operations that eliminate mechanical token waste — agents edit by symbol name instead of line numbers, read file structure instead of entire files, and trace call chains in a single operation.

## Current State

M001/S01–S06 complete. The `aft` Rust binary runs as a persistent process with NDJSON protocol, embeds tree-sitter grammars for 6 languages (TypeScript, JavaScript, TSX, Python, Rust, Go), and handles `outline`, `zoom`, `checkpoint`, `restore_checkpoint`, `list_checkpoints`, `undo`, `edit_history`, `write`, `edit_symbol`, `edit_match`, and `batch` commands. All four mutation commands auto-backup before modification, validate syntax via tree-sitter re-parse, and return structured disambiguation when targets are ambiguous. The OpenCode TypeScript plugin (`opencode-plugin-aft/`) registers all 11 commands as tools with Zod schemas, managing binary lifecycle through a BinaryBridge class with crash recovery and clean shutdown. 142 tests pass (98 Rust unit + 35 Rust integration + 9 plugin integration). Next: S07 (binary distribution — npm platform packages, CI cross-compilation).

## Architecture / Key Patterns

- **Persistent binary process:** The Rust binary runs as a long-lived process, receiving JSON commands on stdin and writing JSON responses to stdout. Keeps tree-sitter parse state, call graph cache, checkpoint store, and edit history in memory.
- **LSP-aware provider interface:** Symbol resolution has a clean provider abstraction — tree-sitter is the default backend, LSP-derived data can be injected via optional `lsp_hints` fields in command JSON.
- **Plugin as bridge, not brain:** The TypeScript plugin manages binary lifecycle (spawn, health, restart), registers tools with OpenCode, and mediates LSP data. All logic lives in the Rust binary.
- **Web-first language priority:** TS/JS/TSX first (shared query patterns), then Python, then Rust and Go.
- **Binary distribution:** npm platform packages following the esbuild/turbo pattern, with `cargo install aft` fallback.

## Capability Contract

See `.gsd/REQUIREMENTS.md` for the explicit capability contract, requirement status, and coverage mapping.

## Milestone Sequence

- [ ] M001: Foundation — Rust binary, tree-sitter for 6 languages, three-layer editing, safety system, OpenCode plugin, binary distribution
- [ ] M002: Language Intelligence — Import management, scope-aware insertion, compound operations, auto-format, transactions
- [ ] M003: Call Graph Navigation — Lazy/incremental call graph, forward/reverse traces, impact analysis, data flow tracking
- [ ] M004: Refactoring Primitives — Move symbol, extract function, inline symbol, LSP integration
