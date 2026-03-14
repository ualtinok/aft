# Project

## What This Is

Agent File Toolkit (AFT) — a Rust binary + OpenCode TypeScript plugin that gives AI coding agents semantic file manipulation and code navigation primitives. Replaces the read-grep-edit-read cycle with operations that match how agents reason about code: edit by symbol name, trace call paths in one call, manage imports with a single operation, checkpoint/restore without git overhead.

Two components: a Rust binary (`aft`) that embeds tree-sitter grammars for 6 languages and does all computation, and a thin TypeScript OpenCode plugin that bridges tool calls to the binary via JSON over stdin/stdout.

## Core Value

One-call semantic file operations that eliminate mechanical token waste — agents edit by symbol name instead of line numbers, read file structure instead of entire files, and trace call chains in a single operation.

## Current State

**M001 (Foundation), M002 (Language Intelligence) complete. M003/S01 (Call Graph Infrastructure), M003/S02 (Reverse Callers + File Watcher), and M003/S03 (Trace to Entry Points) complete.** The `aft` Rust binary runs as a persistent process with NDJSON protocol, embeds tree-sitter grammars for 6 languages (TypeScript, JavaScript, TSX, Python, Rust, Go), and handles 24 domain commands: the original 11 from M001, 3 import commands, 5 structure commands, `transaction`, and 4 call graph commands (`configure`, `call_tree`, `callers`, `trace_to`). All 12 mutation commands support `dry_run: true` and auto-format. The call graph engine builds lazily from a configured project root, resolves cross-file calls through import chains, supports forward call trees, reverse caller lookup, and backward trace-to-entry-point traversal. Entry point detection uses generic heuristics (exported functions, main/init patterns, test patterns per language). A `notify` v8 file watcher invalidates stale graph data on file changes. 345 Rust tests + 39 plugin tests pass. Next: S04 (Data Flow Tracking + Impact Analysis).

## Architecture / Key Patterns

- **Persistent binary process:** The Rust binary runs as a long-lived process, receiving JSON commands on stdin and writing JSON responses on stdout. Keeps tree-sitter parse state, checkpoint store, and edit history in memory.
- **AppContext dispatch:** Single `AppContext` struct threads all shared state (LanguageProvider, BackupStore, CheckpointStore, Config) through command dispatch. Handlers receive `(&RawRequest, &AppContext) -> Response`.
- **LSP-aware provider interface:** Symbol resolution has a clean provider abstraction — tree-sitter is the default backend, LSP-derived data can be injected via optional `lsp_hints` fields in command JSON.
- **Plugin as bridge, not brain:** The TypeScript plugin manages binary lifecycle (spawn, health, restart), registers tools with OpenCode, and mediates LSP data. All logic lives in the Rust binary.
- **Web-first language priority:** TS/JS/TSX first (shared query patterns), then Python, then Rust and Go.
- **Binary distribution:** npm platform packages following the esbuild/turbo pattern (`@aft/core` with optionalDependencies on 5 platform packages), CI cross-compilation pipeline, `cargo install aft` fallback.

## Capability Contract

See `.gsd/REQUIREMENTS.md` for the explicit capability contract, requirement status, and coverage mapping.

## Milestone Sequence

- [x] M001: Foundation — Rust binary, tree-sitter for 6 languages, three-layer editing, safety system, OpenCode plugin, binary distribution (155 tests, all passing)
- [x] M002: Language Intelligence — Import management, scope-aware insertion & compound ops, auto-format & validation, dry-run & transactions (294 Rust tests + 39 plugin tests)
- [ ] M003: Call Graph Navigation — Lazy/incremental call graph, forward/reverse traces, impact analysis, data flow tracking
- [ ] M004: Refactoring Primitives — Move symbol, extract function, inline symbol, LSP integration
