/**
 * Renderer coverage for aft_outline + aft_zoom.
 */

/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import {
  formatZoomBatchResult,
  renderOutlineCall,
  renderOutlineResult,
  renderZoomCall,
  renderZoomResult,
} from "../tools/reading.js";
import { makeContext, makeResult, mockTheme, renderToString } from "./render-test-helpers.js";

describe("reading renderers", () => {
  test("renderOutlineCall and renderZoomCall show targets", () => {
    const outline = renderToString(
      renderOutlineCall({ filePath: "src/a.ts" }, mockTheme, makeContext({ filePath: "src/a.ts" })),
    );
    const zoom = renderToString(
      renderZoomCall(
        { filePath: "src/a.ts", symbol: "run" },
        mockTheme,
        makeContext({ filePath: "src/a.ts", symbol: "run" }),
      ),
    );

    expect(outline).toContain("outline");
    expect(outline).toContain("src/a.ts");
    expect(zoom).toContain("zoom");
    expect(zoom).toContain("run");
  });

  test("renderOutlineResult and renderZoomResult show structured output", () => {
    const outline = renderToString(
      renderOutlineResult(
        makeResult("sample.ts\n  E fn run() 1:5\n  - cls Service 7:12"),
        mockTheme,
        makeContext({ filePath: "sample.ts" }),
      ),
    );
    const zoom = renderToString(
      renderZoomResult(
        makeResult("", {
          name: "run",
          kind: "function",
          range: { start_line: 1, end_line: 4 },
          content: "export function run() {\n  return helper();\n}",
          annotations: {
            calls_out: [{ name: "helper", line: 2 }],
            called_by: [{ name: "main", line: 8 }],
          },
        }),
        { filePath: "sample.ts", symbol: "run" },
        mockTheme,
        makeContext({ filePath: "sample.ts", symbol: "run" }),
      ),
    );

    expect(outline).toContain("sample.ts");
    expect(outline).toContain("Service");
    expect(zoom).toContain("run [function]");
    expect(zoom).toContain("helper:2");
    expect(zoom).toContain("main:8");
  });

  test("reading renderers handle error and empty payloads", () => {
    const error = renderToString(
      renderZoomResult(
        makeResult("symbol not found"),
        { filePath: "sample.ts", symbol: "run" },
        mockTheme,
        makeContext({ filePath: "sample.ts", symbol: "run" }, { isError: true }),
      ),
    );
    const empty = renderToString(
      renderOutlineResult(makeResult(""), mockTheme, makeContext({ directory: "." })),
    );

    expect(error).toContain("symbol not found");
    expect(empty).toContain("No outline available");
  });

  test("batched zoom keeps successes visible when another symbol fails", () => {
    const batch = formatZoomBatchResult(
      ["run", "Missing"],
      [
        { success: true, content: "export function run() {}" },
        { success: false, message: "symbol not found" },
      ],
    );
    const rendered = renderToString(
      renderZoomResult(
        makeResult(batch.text, batch),
        { filePath: "sample.ts", symbols: ["run", "Missing"] },
        mockTheme,
        makeContext({ filePath: "sample.ts", symbols: ["run", "Missing"] }),
      ),
    );

    expect(batch.complete).toBe(false);
    expect(batch.text).toContain("Incomplete zoom results");
    expect(batch.text).toContain("export function run() {}");
    expect(batch.text).toContain('Symbol "Missing" not found: symbol not found');
    expect(rendered).toContain("Incomplete zoom results");
    expect(rendered).toContain("export function run() {}");
    expect(rendered).toContain("symbol not found");
  });
});
