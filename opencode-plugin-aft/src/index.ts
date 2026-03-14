import type { Plugin } from "@opencode-ai/plugin";
import { BinaryBridge } from "./bridge.js";
import { findBinary } from "./resolver.js";
import { readingTools } from "./tools/reading.js";
import { editingTools } from "./tools/editing.js";
import { safetyTools } from "./tools/safety.js";

/**
 * OpenCode plugin for AFT (Agent File Tools).
 *
 * Spawns the `aft` binary as a persistent child process and exposes all 11
 * agent-facing commands as OpenCode tools. The binary communicates via NDJSON
 * over stdin/stdout.
 *
 * Tool categories:
 * - Reading: outline, zoom
 * - Editing: write, edit_symbol, edit_match, batch
 * - Safety: undo, edit_history, checkpoint, restore_checkpoint, list_checkpoints
 */
const plugin: Plugin = async (input) => {
  const binaryPath = findBinary();
  const bridge = new BinaryBridge(binaryPath, input.directory);

  return {
    tool: {
      ...readingTools(bridge),
      ...editingTools(bridge),
      ...safetyTools(bridge),
    },
  };
};

export default plugin;
