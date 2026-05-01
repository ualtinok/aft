import { describe, expect, test } from "bun:test";
import type { AftConfig } from "../config.js";
import { buildHintsFromConfig, buildWorkflowHints } from "../workflow-hints.js";

describe("buildWorkflowHints", () => {
  test("renders all four sections at tool_surface=all with bg + semantic enabled", () => {
    const out = buildWorkflowHints({
      toolSurface: "all",
      hoistBuiltins: true,
      semanticEnabled: true,
      bashBackgroundEnabled: true,
      disabledTools: new Set(),
    });
    expect(out).not.toBeNull();
    expect(out).toContain("## Prefer AFT tools for token efficiency");
    expect(out).toContain("**Web/URL access**");
    expect(out).toContain("**Code exploration**");
    expect(out).toContain("`grep` or `aft_search`");
    expect(out).toContain("Use `aft_navigate`");
    expect(out).toContain("- `callers`");
    expect(out).toContain("- `impact`");
    expect(out).toContain("- `trace_to`");
    expect(out).toContain("- `trace_data`");
    expect(out).toContain("**Long-running commands**");
    expect(out).toContain("`bash({ background: true })`");
  });

  test("omits the bg-bash section when experimental.bash.background is off", () => {
    const out = buildWorkflowHints({
      toolSurface: "recommended",
      hoistBuiltins: true,
      semanticEnabled: false,
      bashBackgroundEnabled: false,
      disabledTools: new Set(),
    });
    expect(out).not.toBeNull();
    expect(out).not.toContain("**Long-running commands**");
    expect(out).not.toContain("background: true");
  });

  test("omits the navigate section at tool_surface=recommended", () => {
    const out = buildWorkflowHints({
      toolSurface: "recommended",
      hoistBuiltins: true,
      semanticEnabled: false,
      bashBackgroundEnabled: false,
      disabledTools: new Set(),
    });
    expect(out).not.toContain("Use `aft_navigate`");
    expect(out).not.toContain("- `callers`");
  });

  test("uses aft_grep when hoist_builtin_tools is false", () => {
    const out = buildWorkflowHints({
      toolSurface: "recommended",
      hoistBuiltins: false,
      semanticEnabled: false,
      bashBackgroundEnabled: false,
      disabledTools: new Set(),
    });
    expect(out).toContain("`aft_grep`");
    expect(out).not.toContain("`grep` to locate");
  });

  test("references aft_search only when semantic is enabled", () => {
    const off = buildWorkflowHints({
      toolSurface: "recommended",
      hoistBuiltins: true,
      semanticEnabled: false,
      bashBackgroundEnabled: false,
      disabledTools: new Set(),
    });
    expect(off).not.toContain("aft_search");

    const on = buildWorkflowHints({
      toolSurface: "recommended",
      hoistBuiltins: true,
      semanticEnabled: true,
      bashBackgroundEnabled: false,
      disabledTools: new Set(),
    });
    expect(on).toContain("aft_search");
  });

  test("returns null at minimal surface — only safety tool present", () => {
    // At minimal surface, aft_outline + aft_zoom may still be present, but
    // grep is not. Code-exploration section needs both. URL section still
    // works on outline+zoom alone, so we get a non-null block. Test the
    // truly empty case:
    const empty = buildWorkflowHints({
      toolSurface: "minimal",
      hoistBuiltins: true,
      semanticEnabled: false,
      bashBackgroundEnabled: false,
      disabledTools: new Set(["aft_outline", "aft_zoom"]),
    });
    expect(empty).toBeNull();
  });

  test("section guarded by disabledTools", () => {
    const out = buildWorkflowHints({
      toolSurface: "all",
      hoistBuiltins: true,
      semanticEnabled: true,
      bashBackgroundEnabled: true,
      disabledTools: new Set(["aft_navigate", "bash_status"]),
    });
    // navigate + bg-bash sections gated off.
    expect(out).not.toContain("Use `aft_navigate`");
    expect(out).not.toContain("**Long-running commands**");
    // Other sections survive.
    expect(out).toContain("**Web/URL access**");
    expect(out).toContain("**Code exploration**");
  });
});

describe("buildHintsFromConfig", () => {
  test("emits hints by default", () => {
    const config: AftConfig = { tool_surface: "recommended" };
    const out = buildHintsFromConfig(config, new Set());
    expect(out).not.toBeNull();
    expect(out).toContain("## Prefer AFT tools for token efficiency");
  });

  test("honors hoist_builtin_tools=false (uses aft_grep)", () => {
    const config: AftConfig = { tool_surface: "recommended", hoist_builtin_tools: false };
    const out = buildHintsFromConfig(config, new Set());
    expect(out).toContain("`aft_grep`");
  });

  test("conditionally appends bg-bash when experimental.bash.background=true", () => {
    const off: AftConfig = { tool_surface: "recommended" };
    expect(buildHintsFromConfig(off, new Set())).not.toContain("**Long-running commands**");

    const on: AftConfig = {
      tool_surface: "recommended",
      experimental: { bash: { background: true } },
    };
    expect(buildHintsFromConfig(on, new Set())).toContain("**Long-running commands**");
  });
});
