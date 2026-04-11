/// <reference path="../../bun-test.d.ts" />
import { describe, expect, test } from "bun:test";
import { homedir, userInfo } from "node:os";

describe("sanitizeContent", () => {
  test("redacts usernames and home paths", async () => {
    const { sanitizeContent } = await import("../../cli/logs.js");
    const username = userInfo().username;
    const home = homedir();

    const input = [
      `user=${username}`,
      `home=${home}/project`,
      "/Users/alice/workspace/file.ts",
      "/home/bob/workspace/file.ts",
      `C:\\Users\\${username}\\repo\\file.ts`,
    ].join("\n");

    const sanitized = sanitizeContent(input);

    expect(sanitized).not.toContain(username);
    expect(sanitized).toContain("user=<USER>");
    expect(sanitized).toContain("home=~/project");
    expect(sanitized).toContain("/Users/<USER>/workspace/file.ts");
    expect(sanitized).toContain("/home/<USER>/workspace/file.ts");
    expect(sanitized).toContain("C:\\Users\\<USER>\\repo\\file.ts");
  });

  test("redacts paths inside JSON-quoted diagnostic bodies", async () => {
    // Regression: the diagnostic report renders paths inside JSON blocks like
    //   { "path": "/Users/ufukaltinok/.cache/opencode/packages/..." }
    // The sanitizer must also strip usernames embedded between quotes.
    const { sanitizeContent } = await import("../../cli/logs.js");

    const input = [
      "{",
      '  "path": "/Users/ufukaltinok/.cache/opencode/packages/@cortexkit/aft-opencode@latest",',
      '  "cached": "0.11.0"',
      "}",
      "- Log file: /home/dave/.local/share/aft/aft-plugin.log (24 KB)",
    ].join("\n");

    const sanitized = sanitizeContent(input);

    expect(sanitized).not.toContain("ufukaltinok");
    expect(sanitized).not.toContain("dave");
    expect(sanitized).toContain(
      "/Users/<USER>/.cache/opencode/packages/@cortexkit/aft-opencode@latest",
    );
    expect(sanitized).toContain("/home/<USER>/.local/share/aft/aft-plugin.log");
  });

  test("sanitizeLogContent alias still works", async () => {
    const { sanitizeLogContent } = await import("../../cli/logs.js");
    expect(sanitizeLogContent("/Users/alice/file")).toBe("/Users/<USER>/file");
  });
});
