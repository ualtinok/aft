/**
 * Audit-3 v0.17 #5: GitHub download URL hostname allowlist.
 *
 * `browser_download_url` returned by the GitHub API is attacker-
 * controllable: a malicious or compromised release record could swap the
 * URL to point at an arbitrary host. We refuse to download anything
 * outside the github.com / githubusercontent.com family before a single
 * byte hits the wire.
 *
 * The allowlist is test-exported via `_assertAllowedDownloadUrlForTesting`
 * because production callers reach it inline through `downloadFile`.
 */

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

  test("accepts objects.githubusercontent.com (GitHub's redirect target)", () => {
    expect(() =>
      assertAllowedDownloadUrl(
        "https://objects.githubusercontent.com/github-production-release-asset-2e65be/123/abc",
      ),
    ).not.toThrow();
  });

  test("accepts api.github.com (release JSON)", () => {
    expect(() =>
      assertAllowedDownloadUrl("https://api.github.com/repos/clangd/clangd/releases/latest"),
    ).not.toThrow();
  });

  test("rejects an attacker-controlled host", () => {
    expect(() => assertAllowedDownloadUrl("https://evil.example/payload.zip")).toThrow(
      /not in the GitHub allowlist/,
    );
  });

  test("rejects http (downgrade attack)", () => {
    expect(() =>
      assertAllowedDownloadUrl("http://github.com/clangd/clangd/releases/download/x.zip"),
    ).toThrow(/must be https/);
  });

  test("rejects file:// URLs (local exfil/escalation)", () => {
    expect(() => assertAllowedDownloadUrl("file:///etc/passwd")).toThrow(/must be https/);
  });

  test("rejects unparseable URL strings", () => {
    expect(() => assertAllowedDownloadUrl("not a url")).toThrow(/not a valid URL/);
  });

  test("rejects subdomain confusion (github.com.evil.example)", () => {
    expect(() => assertAllowedDownloadUrl("https://github.com.evil.example/payload")).toThrow(
      /not in the GitHub allowlist/,
    );
  });

  test("is case-insensitive on hostname (GITHUB.COM)", () => {
    expect(() =>
      assertAllowedDownloadUrl("https://GITHUB.COM/clangd/clangd/releases/download/x.zip"),
    ).not.toThrow();
  });

  test("rejects empty string", () => {
    expect(() => assertAllowedDownloadUrl("")).toThrow();
  });
});
