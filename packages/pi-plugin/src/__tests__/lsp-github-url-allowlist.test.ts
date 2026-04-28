/** Audit-3 v0.17 #5: same allowlist test as OpenCode plugin. */

import { describe, expect, test } from "bun:test";
import { _assertAllowedDownloadUrlForTesting as assertAllowedDownloadUrl } from "../lsp-github-install.js";

describe("downloadFile URL allowlist (audit-3 #5)", () => {
  test("accepts canonical github.com release-asset URL", () => {
    expect(() =>
      assertAllowedDownloadUrl(
        "https://github.com/clangd/clangd/releases/download/18.1.3/clangd-mac-18.1.3.zip",
      ),
    ).not.toThrow();
  });

  test("accepts objects.githubusercontent.com", () => {
    expect(() =>
      assertAllowedDownloadUrl(
        "https://objects.githubusercontent.com/github-production-release-asset-2e65be/123/abc",
      ),
    ).not.toThrow();
  });

  test("rejects an attacker-controlled host", () => {
    expect(() => assertAllowedDownloadUrl("https://evil.example/payload.zip")).toThrow(
      /not in the GitHub allowlist/,
    );
  });

  test("rejects http (downgrade attack)", () => {
    expect(() => assertAllowedDownloadUrl("http://github.com/x.zip")).toThrow(/must be https/);
  });

  test("rejects file:// URLs", () => {
    expect(() => assertAllowedDownloadUrl("file:///etc/passwd")).toThrow(/must be https/);
  });

  test("rejects subdomain confusion", () => {
    expect(() => assertAllowedDownloadUrl("https://github.com.evil.example/x")).toThrow(
      /not in the GitHub allowlist/,
    );
  });

  test("is case-insensitive on hostname", () => {
    expect(() => assertAllowedDownloadUrl("https://GITHUB.COM/x.zip")).not.toThrow();
  });
});
