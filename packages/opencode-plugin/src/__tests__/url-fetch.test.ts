/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, mock, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const tempRoots = new Set<string>();

function makeStorageDir(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-url-fetch-"));
  tempRoots.add(root);
  return join(root, "storage");
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("fetchUrlToTempFile", () => {
  test("blocks redirects to private hosts by default", async () => {
    const { fetchUrlToTempFile } = await import("../shared/url-fetch.js");

    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(
      async () =>
        new Response(null, { status: 302, headers: { location: "http://127.0.0.1/internal" } }),
    ) as unknown as typeof fetch;

    try {
      await expect(
        fetchUrlToTempFile("http://93.184.216.34/start", makeStorageDir()),
      ).rejects.toThrow("Blocked private URL host");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});

describe("isPrivateIp", () => {
  test("blocks IPv4 private ranges", async () => {
    const { _isPrivateIpv4 } = await import("../shared/url-fetch.js");
    expect(_isPrivateIpv4("0.0.0.0")).toBe(true);
    expect(_isPrivateIpv4("10.0.0.1")).toBe(true);
    expect(_isPrivateIpv4("127.0.0.1")).toBe(true);
    expect(_isPrivateIpv4("169.254.169.254")).toBe(true); // AWS metadata
    expect(_isPrivateIpv4("172.16.0.1")).toBe(true);
    expect(_isPrivateIpv4("172.31.255.255")).toBe(true);
    expect(_isPrivateIpv4("192.168.1.1")).toBe(true);
    expect(_isPrivateIpv4("224.0.0.1")).toBe(true); // multicast
    expect(_isPrivateIpv4("255.255.255.255")).toBe(true);
  });

  test("allows IPv4 public addresses", async () => {
    const { _isPrivateIpv4 } = await import("../shared/url-fetch.js");
    expect(_isPrivateIpv4("8.8.8.8")).toBe(false);
    expect(_isPrivateIpv4("93.184.216.34")).toBe(false);
    expect(_isPrivateIpv4("172.15.0.1")).toBe(false); // just outside 172.16/12
    expect(_isPrivateIpv4("172.32.0.1")).toBe(false);
    expect(_isPrivateIpv4("169.253.0.1")).toBe(false);
  });

  test("rejects IPv4-mapped IPv6 loopback bypass via SSRF guard", async () => {
    const { fetchUrlToTempFile } = await import("../shared/url-fetch.js");

    const originalFetch = globalThis.fetch;
    // Redirect to IPv4-mapped IPv6 form of 127.0.0.1
    globalThis.fetch = mock(
      async () =>
        new Response(null, {
          status: 302,
          headers: { location: "http://[::ffff:127.0.0.1]/internal" },
        }),
    ) as unknown as typeof fetch;

    try {
      await expect(
        fetchUrlToTempFile("http://93.184.216.34/start", makeStorageDir()),
      ).rejects.toThrow("Blocked private URL host");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("rejects IPv4-compatible IPv6 loopback bypass via SSRF guard", async () => {
    const { fetchUrlToTempFile } = await import("../shared/url-fetch.js");

    const originalFetch = globalThis.fetch;
    // IPv4-compatible form: ::127.0.0.1 (last colon is at index 1)
    globalThis.fetch = mock(
      async () =>
        new Response(null, {
          status: 302,
          headers: { location: "http://[::127.0.0.1]/internal" },
        }),
    ) as unknown as typeof fetch;

    try {
      await expect(
        fetchUrlToTempFile("http://93.184.216.34/start", makeStorageDir()),
      ).rejects.toThrow("Blocked private URL host");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("rejects IPv6 unspecified address ::", async () => {
    const { fetchUrlToTempFile } = await import("../shared/url-fetch.js");

    const originalFetch = globalThis.fetch;
    globalThis.fetch = mock(
      async () =>
        new Response(null, {
          status: 302,
          headers: { location: "http://[::]/internal" },
        }),
    ) as unknown as typeof fetch;

    try {
      await expect(
        fetchUrlToTempFile("http://93.184.216.34/start", makeStorageDir()),
      ).rejects.toThrow("Blocked private URL host");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});
