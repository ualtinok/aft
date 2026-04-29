import type {
  AgentToolResult,
  ExtensionAPI,
  ExtensionContext,
  Theme,
} from "@mariozechner/pi-coding-agent";
import { Container, Spacer, Text } from "@mariozechner/pi-tui";
import { type Static, Type } from "@sinclair/typebox";
import { trackBgTask } from "../bg-notifications.js";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, resolveSessionId } from "./_shared.js";

const DEFAULT_BASH_TIMEOUT_MS = 120_000;
const BASH_TRANSPORT_TIMEOUT_OVERHEAD_MS = 5_000;

// Background task completion metadata shape (from Track D)
interface BgCompletion {
  task_id: string;
  status: "completed" | "failed" | "cancelled";
  exit_code?: number;
  command?: string;
}

// BashSpawnHook type — Pi's extension point for modifying bash execution
interface BashSpawnContext {
  command: string;
  cwd?: string;
  env?: Record<string, string>;
}

type BashSpawnHook = (ctx: BashSpawnContext) => BashSpawnContext | Promise<BashSpawnContext>;

const BashParams = Type.Object({
  command: Type.String({
    description: "Shell command to execute. Supports pipes, redirections, and shell syntax.",
  }),
  timeout: Type.Optional(
    Type.Number({
      description:
        "Maximum execution time in milliseconds. Default: 120000 (2 minutes). Commands exceeding this are terminated with SIGKILL.",
    }),
  ),
  workdir: Type.Optional(
    Type.String({
      description:
        "Working directory for command execution. Relative paths resolve against the project root. Defaults to the current session's working directory.",
    }),
  ),
  description: Type.Optional(
    Type.String({
      description:
        "Human-readable description shown in UI logs. Helps users understand what the command does without reading shell syntax.",
    }),
  ),
  background: Type.Optional(
    Type.Boolean({
      description:
        "Spawn command in background and return immediately with a task_id. Use bash_status to poll completion and bash_kill to terminate. Ideal for long-running tasks like builds or dev servers.",
    }),
  ),
  compressed: Type.Optional(
    Type.Boolean({
      description:
        "Compress output by removing ANSI codes, carriage returns, and excessive blank lines. Default: true. Set to false for raw terminal output including color codes.",
    }),
  ),
});

const BashTaskParams = Type.Object({
  task_id: Type.String({
    description: "Background bash task id returned by bash({ background: true }).",
  }),
});

interface BashDetails {
  exit_code?: number;
  duration_ms?: number;
  truncated?: boolean;
  output_path?: string;
  task_id?: string;
  bg_completions?: BgCompletion[];
}

interface BashStatusDetails {
  success: boolean;
  status: string;
  exit_code?: number;
  output_preview?: string;
  command?: string;
}

interface BashKillDetails {
  success: boolean;
  status: string;
}

/** Local shape for Pi's render context — mirrors hoisted.ts pattern. */
interface RenderContextLike {
  lastComponent: import("@mariozechner/pi-tui").Component | undefined;
  isError: boolean;
}

/** Truncate output to last N visual lines for terminal width. */
function truncateToVisualLines(text: string, maxLines: number): string {
  const lines = text.split("\n");
  if (lines.length <= maxLines) return text;
  return lines.slice(-maxLines).join("\n");
}

/** Reuse a compatible Text component from last render, or create fresh. */
function reuseText(last: import("@mariozechner/pi-tui").Component | undefined): Text {
  return last instanceof Text ? last : new Text("", 0, 0);
}

/** Reuse a compatible Container from last render, or create fresh. */
function reuseContainer(last: import("@mariozechner/pi-tui").Component | undefined): Container {
  return last instanceof Container ? last : new Container();
}

/** Extract BashSpawnHook from ExtensionAPI if available. */
function getBashSpawnHook(pi: ExtensionAPI): BashSpawnHook | undefined {
  // Pi exposes hooks via getHook() or similar — defensive access
  const api = pi as unknown as {
    getHook?: (name: string) => BashSpawnHook | undefined;
    hooks?: { bashSpawn?: BashSpawnHook };
  };
  if (typeof api.getHook === "function") {
    return api.getHook("bashSpawn");
  }
  return api.hooks?.bashSpawn;
}

export function registerBashTool(pi: ExtensionAPI, ctx: PluginContext): void {
  const spawnHook = getBashSpawnHook(pi);

  pi.registerTool<typeof BashParams, BashDetails>({
    name: "bash",
    label: "bash",
    description:
      "Execute shell commands through AFT's Rust bash handler. By default, output is compressed. Pass `compressed: false` for raw output. Pass `background: true` to spawn in the background and get a task_id for `bash_status`/`bash_kill`.",
    promptSnippet:
      "Run shell commands (timeout in milliseconds; supports workdir, background tasks, compressed output)",
    promptGuidelines: [
      "Use bash only when a dedicated AFT tool is not a better fit.",
      "Prefer background: true for commands that may take longer than 30 seconds.",
      "Set compressed: false when you need ANSI color codes in the output.",
    ],
    parameters: BashParams,
    async execute(_toolCallId, params: Static<typeof BashParams>, _signal, onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);

      // Build spawn context for potential hook modification
      let spawnContext: BashSpawnContext = {
        command: params.command,
        cwd: params.workdir,
      };

      // Apply BashSpawnHook if available (Pi extension point)
      if (spawnHook) {
        try {
          spawnContext = await spawnHook(spawnContext);
        } catch (hookErr) {
          // Hook errors should not silently fail — surface them
          throw new Error(
            `BashSpawnHook failed: ${hookErr instanceof Error ? hookErr.message : String(hookErr)}`,
          );
        }
      }

      let streamed = "";
      const response = await callBridge(
        bridge,
        "bash",
        {
          command: spawnContext.command,
          timeout: params.timeout,
          workdir: spawnContext.cwd ?? params.workdir,
          env: spawnContext.env,
          description: params.description,
          background: params.background,
          compressed: params.compressed,
        },
        extCtx,
        {
          transportTimeoutMs: bashTransportTimeoutMs(params.timeout),
          onProgress: ({ text }) => {
            streamed += text;
            // Stream truncated output to avoid overwhelming the UI
            const displayText = truncateToVisualLines(streamed, 100);
            onUpdate?.(bashResult(displayText, { streaming: true }));
          },
        },
      ).catch((err) => {
        if (err instanceof Error && err.message.includes("permission_required")) {
          // Pi has no permission system — this should never reach us from Rust
          // (Track C scan returns empty for Pi). If it somehow did, throw clearly.
          throw new Error(
            "Permission ask reached Pi adapter — this is a bug. Pi has no permission system.",
          );
        }
        throw err;
      });

      if (response.success === false) {
        throw new Error((response.message as string | undefined) ?? "bash failed");
      }

      const taskId = response.task_id as string | undefined;
      if (response.status === "running" && taskId) {
        trackBgTask(resolveSessionId(extCtx), taskId);
      }

      const details: BashDetails = {
        exit_code: response.exit_code as number | undefined,
        duration_ms: response.duration_ms as number | undefined,
        truncated: response.truncated as boolean | undefined,
        output_path: response.output_path as string | undefined,
        task_id: taskId,
      };

      const output = (response.output as string | undefined) ?? "";
      return bashResult(output, details);
    },
    renderCall(args, theme, context) {
      return renderBashCall(args?.command, args?.description, theme, context);
    },
    renderResult(result, _options, theme, context) {
      return renderBashResult(result, theme, context);
    },
  });

  pi.registerTool<typeof BashTaskParams, BashStatusDetails>(createBashStatusTool(ctx));
  pi.registerTool<typeof BashTaskParams, BashKillDetails>(createBashKillTool(ctx));
}

function bashTransportTimeoutMs(timeout: number | undefined): number {
  const bashTimeout = timeout ?? DEFAULT_BASH_TIMEOUT_MS;
  return Math.max(30_000, bashTimeout + BASH_TRANSPORT_TIMEOUT_OVERHEAD_MS);
}

export function createBashStatusTool(ctx: PluginContext) {
  return {
    name: "bash_status",
    label: "bash_status",
    description:
      "Check the status of a background bash task spawned with bash({ background: true }).",
    promptSnippet: "Poll a background bash task by task_id",
    parameters: BashTaskParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof BashTaskParams>,
      _signal: AbortSignal | undefined,
      _onUpdate: ((update: AgentToolResult<BashStatusDetails>) => void) | undefined,
      extCtx: ExtensionContext,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const data = await callBridge(bridge, "bash_status", { task_id: params.task_id }, extCtx);
      if (data.success === false) {
        throw new Error((data.message as string | undefined) ?? "bash_status failed");
      }
      const details = data as unknown as BashStatusDetails;
      return bashStatusResult(formatBashStatus(params.task_id, details), details);
    },
  };
}

export function createBashKillTool(ctx: PluginContext) {
  return {
    name: "bash_kill",
    label: "bash_kill",
    description:
      "Terminate a running background bash task spawned with bash({ background: true }).",
    promptSnippet: "Kill a background bash task by task_id",
    parameters: BashTaskParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof BashTaskParams>,
      _signal: AbortSignal | undefined,
      _onUpdate: ((update: AgentToolResult<BashKillDetails>) => void) | undefined,
      extCtx: ExtensionContext,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const data = await callBridge(bridge, "bash_kill", { task_id: params.task_id }, extCtx);
      if (data.success === false) {
        throw new Error((data.message as string | undefined) ?? "bash_kill failed");
      }
      const details = data as unknown as BashKillDetails;
      return bashKillResult(`Task ${params.task_id}: killed`, details);
    },
  };
}

function bashResult(
  output: string,
  details: Partial<BashDetails> & { streaming?: boolean },
): AgentToolResult<BashDetails> {
  return {
    content: [{ type: "text", text: output }],
    details: {
      exit_code: details.exit_code,
      duration_ms: details.duration_ms,
      truncated: details.truncated,
      output_path: details.output_path,
      task_id: details.task_id,
      bg_completions: details.bg_completions,
    } as BashDetails,
  };
}

function bashStatusResult(
  output: string,
  details: BashStatusDetails,
): AgentToolResult<BashStatusDetails> {
  return {
    content: [{ type: "text", text: output }],
    details,
  };
}

function bashKillResult(
  output: string,
  details: BashKillDetails,
): AgentToolResult<BashKillDetails> {
  return {
    content: [{ type: "text", text: output }],
    details,
  };
}

function formatBashStatus(taskId: string, details: BashStatusDetails): string {
  const exit = typeof details.exit_code === "number" ? ` (exit ${details.exit_code})` : "";
  let text = `Task ${taskId}: ${details.status}${exit}`;
  if (isTerminalStatus(details.status) && details.output_preview) {
    text += `\n${details.output_preview.slice(0, 200)}`;
  }
  return text;
}

function isTerminalStatus(status: string): boolean {
  return status !== "running";
}

function renderBashCall(
  command: string | undefined,
  description: string | undefined,
  theme: Theme,
  context: RenderContextLike,
): Text {
  const text = reuseText(context.lastComponent);
  const display = description ?? (command ? shortenCommand(command) : "...");
  text.setText(`${theme.fg("toolTitle", theme.bold("bash"))} ${theme.fg("accent", display)}`);
  return text;
}

function renderBashResult(
  result: AgentToolResult<BashDetails>,
  theme: Theme,
  context: RenderContextLike,
): import("@mariozechner/pi-tui").Component {
  // Errors: red text with error details
  if (context.isError) {
    const errorText = result.content
      .filter((c) => c.type === "text")
      .map((c) => (c as { text?: string }).text ?? "")
      .join("\n")
      .trim();
    const text = reuseText(context.lastComponent);
    text.setText(`\n${theme.fg("error", errorText || "bash failed")}`);
    return text;
  }

  const details = result.details;
  const exitCode = details?.exit_code;
  const bgCompletions = details?.bg_completions ?? [];

  // Build result display
  const container = reuseContainer(context.lastComponent);
  container.clear();
  container.addChild(new Spacer(1));

  // Exit code indicator
  if (exitCode !== undefined) {
    const exitColor = exitCode === 0 ? "success" : "error";
    const exitText = theme.fg(exitColor, `exit ${exitCode}`);
    container.addChild(new Text(exitText, 1, 0));
  }

  // Background completions notification (from Track D metadata)
  if (bgCompletions.length > 0) {
    container.addChild(new Spacer(1));
    for (const bg of bgCompletions) {
      const cmdPreview = bg.command ? bg.command.slice(0, 60) : "unknown command";
      const suffix = (bg.command?.length ?? 0) > 60 ? "..." : "";
      const exitInfo = bg.exit_code !== undefined ? `exit ${bg.exit_code}` : bg.status;
      const statusColor = bg.status === "completed" && bg.exit_code === 0 ? "success" : "warning";
      const line = theme.fg(
        statusColor,
        `Background task ${bg.task_id} completed (${exitInfo}): ${cmdPreview}${suffix}`,
      );
      container.addChild(new Text(line, 1, 0));
    }
  }

  // Duration info (muted)
  if (details?.duration_ms !== undefined) {
    container.addChild(new Spacer(1));
    const durationText = theme.fg("muted", `${details.duration_ms}ms`);
    container.addChild(new Text(durationText, 1, 0));
  }

  // Truncation notice
  if (details?.truncated) {
    container.addChild(new Spacer(1));
    const truncText = theme.fg("warning", "(output truncated)");
    container.addChild(new Text(truncText, 1, 0));
  }

  return container;
}

function shortenCommand(command: string): string {
  // Truncate long commands for UI display
  if (command.length <= 60) return command;
  return `${command.slice(0, 57)}...`;
}
