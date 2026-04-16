#!/usr/bin/env bash
# AFT Claude Code Hooks Uninstaller
# Removes AFT hooks from Claude Code

set -euo pipefail

CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

# Remove hook files
[ -f "$HOOKS_DIR/aft" ] && rm "$HOOKS_DIR/aft" && info "Removed $HOOKS_DIR/aft"
[ -f "$HOOKS_DIR/aft-hook.sh" ] && rm "$HOOKS_DIR/aft-hook.sh" && info "Removed $HOOKS_DIR/aft-hook.sh"

# Remove AFT.md
[ -f "$CLAUDE_DIR/AFT.md" ] && rm "$CLAUDE_DIR/AFT.md" && info "Removed $CLAUDE_DIR/AFT.md"

# Remove @AFT.md from CLAUDE.md
if [ -f "$CLAUDE_DIR/CLAUDE.md" ]; then
    if grep -q "@AFT.md" "$CLAUDE_DIR/CLAUDE.md"; then
        sed -i '' '/@AFT.md/d' "$CLAUDE_DIR/CLAUDE.md"
        info "Removed @AFT.md from CLAUDE.md"
    fi
fi

# Remove hooks from settings.json
SETTINGS_FILE="$CLAUDE_DIR/settings.json"
if [ -f "$SETTINGS_FILE" ] && command -v jq &>/dev/null; then
    TEMP_FILE=$(mktemp)
    jq '
      .hooks.PreToolUse = [
        .hooks.PreToolUse[] | select(.hooks[].command | contains("aft-hook.sh") | not)
      ]
    ' "$SETTINGS_FILE" > "$TEMP_FILE" 2>/dev/null && mv "$TEMP_FILE" "$SETTINGS_FILE" && \
        info "Removed AFT hooks from settings.json"
fi

# Remove symlink
[ -L "/usr/local/bin/aft" ] && rm "/usr/local/bin/aft" 2>/dev/null && info "Removed /usr/local/bin/aft symlink"

echo ""
echo -e "${GREEN}AFT Claude Code hooks uninstalled.${NC}"
echo "Restart Claude Code to complete removal."
