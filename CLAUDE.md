# AFT - Agent File Tools

Tree-sitter powered code analysis for massive context savings (60-90% token reduction).

## MANDATORY: Always Use AFT First

**CRITICAL**: AFT semantic commands are the DEFAULT, not optional. Grep/Read with limited context (e.g., 3 lines) misses the bigger picture. We want to SEE the full picture, not shoot in the dark.

**AFT applies to ALL file types** — not just code. Markdown, config, docs, JSON, YAML all benefit. Even for "just checking what files are" — outline first.

**Before reading ANY files:**
1. `aft outline` FIRST - understand structure before diving in
2. `aft zoom` for symbols - never read full files when you need one function
3. `aft callers`/`aft call_tree` for flow - grep misses cross-file relationships

## AFT CLI Commands

Use `aft` commands via Bash for code navigation. These provide structured output optimized for LLM consumption.

### Semantic Commands (USE THESE BY DEFAULT)

```bash
# Get structure without content (~10% of full read tokens)
aft outline <file|directory>

# Inspect symbol with call-graph annotations
aft zoom <file> <symbol>

# Forward call graph - what does this function call?
aft call_tree <file> <symbol>

# Reverse call graph - who calls this function?
aft callers <file> <symbol>

# Impact analysis - what breaks if this changes?
aft impact <file> <symbol>

# Trace analysis - how does execution reach this?
aft trace_to <file> <symbol>
```

### Basic Commands (fallback only)

```bash
aft read <file> [start_line] [limit]   # Read with line numbers
aft grep <pattern> [path]              # Trigram-indexed search
aft glob <pattern> [path]              # File pattern matching
```

## Decision Tree

```
Need to understand files?
    |
    +-- Don't know the file structure?
    |       -> aft outline <dir>
    |
    +-- Checking what files contain (docs, config, etc.)?
    |       -> aft outline <dir>, then selective reads
    |
    +-- Know the file, need specific symbol?
    |       -> aft zoom <file> <symbol>
    |
    +-- Need to understand what calls what?
    |       -> aft call_tree <file> <symbol>
    |
    +-- Need to find all usages?
    |       -> aft callers <file> <symbol>
    |
    +-- Planning a change?
    |       -> aft impact <file> <symbol>
    |
    +-- Debugging how execution reaches a point?
            -> aft trace_to <file> <symbol>
```

## When to Use What

| Task | Command | Token Savings |
|------|---------|---------------|
| Understanding file structure | `aft outline` | ~90% vs full read |
| Checking what docs/configs contain | `aft outline` + selective read | ~80% vs read all |
| Finding function definition | `aft zoom file symbol` | Exact code only |
| Understanding dependencies | `aft call_tree` | Structured graph |
| Finding usage sites | `aft callers` | All call sites |
| Planning refactors | `aft impact` | Change propagation |
| Debugging call paths | `aft trace_to` | Execution paths |

## Rules (NOT suggestions)

1. **ALWAYS start with outline** - Before reading ANY file, use `aft outline` to understand structure
2. **ALWAYS zoom to symbols** - Never read full files when you need specific functions
3. **ALWAYS use call graphs** - For understanding code flow, `call_tree` and `callers` reveal what grep cannot
4. **ALWAYS impact before refactor** - Run `aft impact` before making changes to understand blast radius
5. **NEVER grep with limited context** - If you need more than the symbol name, use AFT semantic commands
6. **ALWAYS outline before sampling** - Even for "just checking what files are" tasks, outline first

## Context Protection

**Context is finite.** Even when a user explicitly requests "contents" or "read all files":

1. **Directory reads: outline first** - For directories with 5+ files, ALWAYS run `aft outline` and confirm which specific files are needed before reading
2. **All file types benefit** - AFT applies to markdown, config, docs, and data files — not just code. Documentation directories especially benefit from outline-first
3. **Batch limit** - Never read more than 3-5 files in a single action without confirming user intent. Context exhaustion breaks the conversation.
4. **User requests don't override physics** - "Read all files" is a request, not a command to fill context. Propose `aft outline` + selective reads instead.

## Supported Languages

TypeScript, JavaScript, Python, Rust, Go, C/C++, Java, Ruby, Markdown

## Hook Integration

Read, Grep, and Glob tools are automatically routed through AFT via hooks for indexed performance.
