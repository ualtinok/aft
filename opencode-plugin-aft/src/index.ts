import type { Plugin } from "@opencode-ai/plugin";
import { BinaryBridge } from "./bridge.js";
import { findBinary } from "./resolver.js";
import { readingTools } from "./tools/reading.js";
import { editingTools } from "./tools/editing.js";
import { safetyTools } from "./tools/safety.js";
import { importTools } from "./tools/imports.js";
import { structureTools } from "./tools/structure.js";
import { transactionTools } from "./tools/transaction.js";
import { navigationTools } from "./tools/navigation.js";

/**
 * OpenCode plugin for AFT (Agent File Tools).
 *
 * Spawns the `aft` binary as a persistent child process and exposes all
 * agent-facing commands as OpenCode tools. The binary communicates via NDJSON
 * over stdin/stdout.
 *
 * Tool categories:
 * - Reading: outline, zoom
 * - Editing: write, edit_symbol, edit_match, batch
 * - Safety: undo, edit_history, checkpoint, restore_checkpoint, list_checkpoints
 * - Imports: add_import, remove_import, organize_imports
 * - Structure: add_member, add_derive, wrap_try_catch, add_decorator, add_struct_tags
 * - Transaction: transaction
 * - Navigation: aft_configure, aft_call_tree
 */
const plugin: Plugin = async (input) => {
  const binaryPath = findBinary();
  const bridge = new BinaryBridge(binaryPath, input.directory);

  return {
    tool: {
      ...readingTools(bridge),
      ...editingTools(bridge),
      ...safetyTools(bridge),
      ...importTools(bridge),
      ...structureTools(bridge),
      ...transactionTools(bridge),
      ...navigationTools(bridge),
    },
  };
};

export default plugin;
