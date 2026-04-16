#!/usr/bin/env bash
# AFT Claude Code Hooks Installer
# Installs AFT hooks for Claude Code integration

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# Check for required tools
command -v jq &>/dev/null || error "jq is required but not installed. Install with: brew install jq"

# Build AFT binary if needed
AFT_BINARY="$AFT_ROOT/target/release/aft"
if [ ! -x "$AFT_BINARY" ]; then
    info "Building AFT binary..."
    cd "$AFT_ROOT"
    cargo build --release || error "Failed to build AFT binary"
fi

info "AFT binary: $AFT_BINARY"

# Create directories
mkdir -p "$HOOKS_DIR"
info "Created hooks directory: $HOOKS_DIR"

# Write aft CLI wrapper
cat > "$HOOKS_DIR/aft" << 'WRAPPER_EOF'
#!/usr/bin/env bash
# AFT CLI wrapper for Claude Code
# Usage: aft <command> [args...]
#
# Commands:
#   outline <file|dir>              - Get file/directory structure (symbols, functions, classes)
#   zoom <file> [symbol]            - Inspect symbol with call-graph annotations
#   call_tree <file> <symbol>       - What does this function call? (forward graph)
#   callers <file> <symbol>         - Who calls this function? (reverse graph)
#   impact <file> <symbol>          - What breaks if this changes?
#   trace_to <file> <symbol>        - How does execution reach this function?
#   read <file> [start] [limit]     - Read file with line numbers
#   grep <pattern> [path]           - Search with trigram index
#   glob <pattern> [path]           - Find files by pattern

set -euo pipefail

AFT_BINARY="__AFT_BINARY_PATH__"
WORK_DIR="${PWD}"

# Check binary exists
if [ ! -x "$AFT_BINARY" ]; then
  echo "Error: AFT binary not found at $AFT_BINARY" >&2
  exit 1
fi

# Send command to AFT binary
call_aft() {
  local cmd="$1"
  shift
  local params="$1"

  local config_req=$(jq -cn --arg root "$WORK_DIR" '{id:"cfg",command:"configure",project_root:$root}')
  local cmd_req=$(echo "$params" | jq -c --arg cmd "$cmd" '{id:"cmd",command:$cmd} + .')

  local result=$( (echo "$config_req"; echo "$cmd_req") | "$AFT_BINARY" 2>/dev/null | grep '"id":"cmd"' | head -1)

  # Check success
  local success=$(echo "$result" | jq -r '.success // false')
  if [ "$success" != "true" ]; then
    local msg=$(echo "$result" | jq -r '.message // "Command failed"')
    echo "Error: $msg" >&2
    exit 1
  fi

  # Output text or content
  local text=$(echo "$result" | jq -r '.text // .content // empty')
  if [ -n "$text" ]; then
    echo "$text"
  else
    echo "$result" | jq .
  fi
}

CMD="${1:-help}"
shift || true

case "$CMD" in
  outline)
    FILE="${1:-}"
    [ -z "$FILE" ] && { echo "Usage: aft outline <file|directory>"; exit 1; }

    # Check if directory - discover source files
    if [ -d "$FILE" ]; then
      FILES=$(find "$FILE" -type f \( -name "*.ts" -o -name "*.tsx" -o -name "*.js" -o -name "*.jsx" \
        -o -name "*.py" -o -name "*.rs" -o -name "*.go" -o -name "*.c" -o -name "*.cpp" -o -name "*.h" \
        -o -name "*.java" -o -name "*.rb" -o -name "*.md" \) \
        ! -path "*/node_modules/*" ! -path "*/.git/*" ! -path "*/target/*" ! -path "*/dist/*" \
        2>/dev/null | head -100 | jq -R . | jq -s .)
      PARAMS=$(jq -cn --argjson files "$FILES" '{files:$files}')
    else
      PARAMS=$(jq -cn --arg f "$FILE" '{file:$f}')
    fi
    call_aft "outline" "$PARAMS"
    ;;

  zoom)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] && { echo "Usage: aft zoom <file> [symbol]"; exit 1; }

    if [ -n "$SYMBOL" ]; then
      PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    else
      PARAMS=$(jq -cn --arg f "$FILE" '{file:$f}')
    fi
    call_aft "zoom" "$PARAMS"
    ;;

  call_tree)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft call_tree <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "call_tree" "$PARAMS"
    ;;

  callers)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft callers <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "callers" "$PARAMS"
    ;;

  impact)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft impact <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "impact" "$PARAMS"
    ;;

  trace_to)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft trace_to <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "trace_to" "$PARAMS"
    ;;

  read)
    FILE="${1:-}"
    START="${2:-1}"
    LIMIT="${3:-2000}"
    [ -z "$FILE" ] && { echo "Usage: aft read <file> [start_line] [limit]"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --argjson s "$START" --argjson l "$LIMIT" \
      '{file:$f,start_line:$s,limit:$l}')
    call_aft "read" "$PARAMS"
    ;;

  grep)
    PATTERN="${1:-}"
    PATH_ARG="${2:-.}"
    [ -z "$PATTERN" ] && { echo "Usage: aft grep <pattern> [path]"; exit 1; }

    PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')
    call_aft "grep" "$PARAMS"
    ;;

  glob)
    PATTERN="${1:-}"
    PATH_ARG="${2:-.}"
    [ -z "$PATTERN" ] && { echo "Usage: aft glob <pattern> [path]"; exit 1; }

    PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')
    call_aft "glob" "$PARAMS"
    ;;

  help|--help|-h)
    cat << 'EOF'
AFT - Agent File Tools (Tree-sitter powered code analysis)

SEMANTIC COMMANDS (massive context savings):
  aft outline <file|dir>          Structure without content (~10% tokens)
  aft zoom <file> <symbol>        Symbol + call graph annotations
  aft call_tree <file> <symbol>   Forward call graph (what does it call?)
  aft callers <file> <symbol>     Reverse call graph (who calls it?)
  aft impact <file> <symbol>      What breaks if this changes?
  aft trace_to <file> <symbol>    How does execution reach this?

BASIC COMMANDS:
  aft read <file> [start] [limit] Read with line numbers
  aft grep <pattern> [path]       Trigram-indexed search
  aft glob <pattern> [path]       File pattern matching

EXAMPLES:
  aft outline src/                # Get structure of all files in src/
  aft zoom main.go main           # Inspect main() with call graph
  aft callers api.go HandleRequest # Find all callers
  aft call_tree service.go Process # See what Process() calls
EOF
    ;;

  *)
    echo "Unknown command: $CMD"
    echo "Run 'aft help' for usage"
    exit 1
    ;;
esac
WRAPPER_EOF

# Replace placeholder with actual binary path
sed -i '' "s|__AFT_BINARY_PATH__|$AFT_BINARY|g" "$HOOKS_DIR/aft"
chmod +x "$HOOKS_DIR/aft"
info "Installed CLI wrapper: $HOOKS_DIR/aft"

# Write aft-hook.sh
cat > "$HOOKS_DIR/aft-hook.sh" << 'HOOK_EOF'
#!/usr/bin/env bash
# AFT Hook for Claude Code
# Intercepts Read, Grep, Glob tools and routes them through AFT binary

TOOL_NAME="${1:-}"
AFT_BINARY="__AFT_BINARY_PATH__"

# Check dependencies
command -v jq &>/dev/null || exit 0
[ -x "$AFT_BINARY" ] || exit 0

# Read input JSON
INPUT=$(cat)
TOOL_INPUT=$(echo "$INPUT" | jq -c '.tool_input // {}')
WORK_DIR=$(echo "$INPUT" | jq -r '.session.workingDirectory // "."')

# Call AFT binary with configure + command
call_aft() {
  local cmd="$1"
  local params="$2"

  local config_req=$(jq -cn --arg root "$WORK_DIR" '{id:"cfg",command:"configure",project_root:$root}')
  local cmd_req=$(echo "$params" | jq -c --arg cmd "$cmd" '{id:"cmd",command:$cmd} + .')

  (echo "$config_req"; echo "$cmd_req") | "$AFT_BINARY" 2>/dev/null | grep '"id":"cmd"' | head -1
}

case "$TOOL_NAME" in
  Read)
    FILE_PATH=$(echo "$TOOL_INPUT" | jq -r '.file_path // empty')
    [ -z "$FILE_PATH" ] && exit 0

    OFFSET=$(echo "$TOOL_INPUT" | jq -r '.offset // 0')
    LIMIT=$(echo "$TOOL_INPUT" | jq -r '.limit // 2000')
    START_LINE=$((OFFSET + 1))

    PARAMS=$(jq -cn --arg f "$FILE_PATH" --argjson s "$START_LINE" --argjson l "$LIMIT" \
      '{file:$f,start_line:$s,limit:$l}')

    RESULT=$(call_aft "read" "$PARAMS")
    [ -z "$RESULT" ] && exit 0

    SUCCESS=$(echo "$RESULT" | jq -r '.success')
    [ "$SUCCESS" != "true" ] && exit 0

    CONTENT=$(echo "$RESULT" | jq -r '.content // empty')
    [ -z "$CONTENT" ] && exit 0

    # Output to stderr for exit 2 blocking message
    echo "[AFT Read] $FILE_PATH" >&2
    echo "$CONTENT" >&2
    exit 2
    ;;

  Grep)
    PATTERN=$(echo "$TOOL_INPUT" | jq -r '.pattern // empty')
    [ -z "$PATTERN" ] && exit 0

    PATH_ARG=$(echo "$TOOL_INPUT" | jq -r '.path // "."')
    INCLUDE=$(echo "$TOOL_INPUT" | jq -r '.include // empty')

    if [ -n "$INCLUDE" ]; then
      PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" --arg i "$INCLUDE" \
        '{pattern:$p,path:$d,include:$i}')
    else
      PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')
    fi

    RESULT=$(call_aft "grep" "$PARAMS")
    [ -z "$RESULT" ] && exit 0

    SUCCESS=$(echo "$RESULT" | jq -r '.success')
    [ "$SUCCESS" != "true" ] && exit 0

    CONTENT=$(echo "$RESULT" | jq -r '.text // empty')
    [ -z "$CONTENT" ] && exit 0

    echo "[AFT Grep] $PATTERN" >&2
    echo "$CONTENT" >&2
    exit 2
    ;;

  Glob)
    PATTERN=$(echo "$TOOL_INPUT" | jq -r '.pattern // empty')
    [ -z "$PATTERN" ] && exit 0

    PATH_ARG=$(echo "$TOOL_INPUT" | jq -r '.path // "."')
    PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')

    RESULT=$(call_aft "glob" "$PARAMS")
    [ -z "$RESULT" ] && exit 0

    SUCCESS=$(echo "$RESULT" | jq -r '.success')
    [ "$SUCCESS" != "true" ] && exit 0

    CONTENT=$(echo "$RESULT" | jq -r '.text // empty')
    [ -z "$CONTENT" ] && exit 0

    echo "[AFT Glob] $PATTERN" >&2
    echo "$CONTENT" >&2
    exit 2
    ;;

  *)
    exit 0
    ;;
esac
HOOK_EOF

sed -i '' "s|__AFT_BINARY_PATH__|$AFT_BINARY|g" "$HOOKS_DIR/aft-hook.sh"
chmod +x "$HOOKS_DIR/aft-hook.sh"
info "Installed hook script: $HOOKS_DIR/aft-hook.sh"

# Write AFT.md instructions
cat > "$CLAUDE_DIR/AFT.md" << 'INSTRUCTIONS_EOF'
# AFT - Agent File Tools

Tree-sitter powered code analysis for massive context savings (60-90% token reduction).

## AFT CLI Commands

Use `aft` commands via Bash for code navigation. These provide structured output optimized for LLM consumption.

### Semantic Commands (prefer these over raw file reads)

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

### Basic Commands

```bash
aft read <file> [start_line] [limit]   # Read with line numbers
aft grep <pattern> [path]              # Trigram-indexed search
aft glob <pattern> [path]              # File pattern matching
```

## When to Use What

| Task | Command | Token Savings |
|------|---------|---------------|
| Understanding file structure | `aft outline` | ~90% vs full read |
| Finding function definition | `aft zoom file symbol` | Exact code only |
| Understanding dependencies | `aft call_tree` | Structured graph |
| Finding usage sites | `aft callers` | All call sites |
| Planning refactors | `aft impact` | Change propagation |
| Debugging call paths | `aft trace_to` | Execution paths |

## Best Practices

1. **Start with outline** - Before reading a file, use `aft outline` to understand structure
2. **Zoom to symbols** - Instead of reading full files, use `aft zoom` for specific functions
3. **Use call graphs** - For understanding code flow, `call_tree` and `callers` are more efficient than grep
4. **Impact before refactor** - Run `aft impact` before making changes to understand blast radius

## Supported Languages

TypeScript, JavaScript, Python, Rust, Go, C/C++, Java, Ruby, Markdown

## Hook Integration

Read, Grep, and Glob tools are automatically routed through AFT via hooks for indexed performance.
INSTRUCTIONS_EOF

info "Installed instructions: $CLAUDE_DIR/AFT.md"

# Update CLAUDE.md to include @AFT.md
if [ -f "$CLAUDE_DIR/CLAUDE.md" ]; then
    if ! grep -q "@AFT.md" "$CLAUDE_DIR/CLAUDE.md"; then
        echo "@AFT.md" >> "$CLAUDE_DIR/CLAUDE.md"
        info "Added @AFT.md to existing CLAUDE.md"
    else
        info "CLAUDE.md already includes @AFT.md"
    fi
else
    echo "@AFT.md" > "$CLAUDE_DIR/CLAUDE.md"
    info "Created CLAUDE.md with @AFT.md"
fi

# Update settings.json with hooks
SETTINGS_FILE="$CLAUDE_DIR/settings.json"

if [ -f "$SETTINGS_FILE" ]; then
    # Check if hooks already exist
    if jq -e '.hooks.PreToolUse[] | select(.matcher == "Read") | .hooks[] | select(.command | contains("aft-hook.sh"))' "$SETTINGS_FILE" &>/dev/null; then
        info "AFT hooks already configured in settings.json"
    else
        # Add AFT hooks to existing PreToolUse array
        TEMP_FILE=$(mktemp)

        jq --arg hooks_dir "$HOOKS_DIR" '
          .hooks.PreToolUse = (
            (.hooks.PreToolUse // []) + [
              {
                "matcher": "Read",
                "hooks": [{"type": "command", "command": ($hooks_dir + "/aft-hook.sh Read")}]
              },
              {
                "matcher": "Grep",
                "hooks": [{"type": "command", "command": ($hooks_dir + "/aft-hook.sh Grep")}]
              },
              {
                "matcher": "Glob",
                "hooks": [{"type": "command", "command": ($hooks_dir + "/aft-hook.sh Glob")}]
              }
            ]
          )
        ' "$SETTINGS_FILE" > "$TEMP_FILE"

        mv "$TEMP_FILE" "$SETTINGS_FILE"
        info "Added AFT hooks to settings.json"
    fi
else
    # Create new settings.json
    cat > "$SETTINGS_FILE" << SETTINGS_EOF
{
  "\$schema": "https://json.schemastore.org/claude-code-settings.json",
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read",
        "hooks": [{"type": "command", "command": "$HOOKS_DIR/aft-hook.sh Read"}]
      },
      {
        "matcher": "Grep",
        "hooks": [{"type": "command", "command": "$HOOKS_DIR/aft-hook.sh Grep"}]
      },
      {
        "matcher": "Glob",
        "hooks": [{"type": "command", "command": "$HOOKS_DIR/aft-hook.sh Glob"}]
      }
    ]
  }
}
SETTINGS_EOF
    info "Created settings.json with AFT hooks"
fi

# Add aft to PATH via symlink
if [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
    ln -sf "$HOOKS_DIR/aft" /usr/local/bin/aft 2>/dev/null && \
        info "Symlinked aft to /usr/local/bin/aft" || \
        warn "Could not symlink to /usr/local/bin (run with sudo if needed)"
else
    warn "Cannot write to /usr/local/bin - add $HOOKS_DIR to PATH manually"
fi

echo ""
echo -e "${GREEN}AFT Claude Code integration installed successfully!${NC}"
echo ""
echo "Installed files:"
echo "  $HOOKS_DIR/aft           - CLI wrapper"
echo "  $HOOKS_DIR/aft-hook.sh   - Tool interceptor"
echo "  $CLAUDE_DIR/AFT.md       - Claude instructions"
echo "  $CLAUDE_DIR/settings.json - Hook configuration"
echo ""
echo "Usage:"
echo "  aft outline src/         # Get file structure"
echo "  aft zoom file.ts func    # Inspect function"
echo "  aft callers file.ts func # Find all callers"
echo ""
echo "Restart Claude Code to activate hooks."
