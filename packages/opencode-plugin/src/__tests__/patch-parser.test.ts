import { describe, expect, test } from "bun:test";
import { applyUpdateChunks, parsePatch, type UpdateFileChunk } from "../patch-parser.js";

describe("parsePatch error paths", () => {
  test("throws when begin/end markers are missing", () => {
    expect(() => parsePatch("*** Add File: hello.txt\n+hello")).toThrow(
      "Invalid patch format: missing *** Begin Patch / *** End Patch markers",
    );
  });

  test("returns an empty list for an empty patch body", () => {
    expect(parsePatch("*** Begin Patch\n*** End Patch")).toEqual([]);
  });

  test("ignores malformed add-file headers with no filename", () => {
    expect(parsePatch("*** Begin Patch\n*** Add File:\n+hello\n*** End Patch")).toEqual([]);
  });

  test("throws when patch text exceeds the size limit", () => {
    const oversizedPatch = "x".repeat(1024 * 1024 + 1);

    expect(() => parsePatch(oversizedPatch)).toThrow(
      `Patch too large: ${oversizedPatch.length} bytes exceeds limit of 1048576 bytes`,
    );
  });

  test("throws when the patch contains more than 500 file operations", () => {
    const patch = ["*** Begin Patch"];
    for (let index = 0; index < 501; index++) {
      patch.push(`*** Add File: file-${index}.txt`);
      patch.push(`+line ${index}`);
    }
    patch.push("*** End Patch");

    expect(() => parsePatch(patch.join("\n"))).toThrow(
      "Patch exceeds maximum of 500 file operations",
    );
  });

  test("treats invalid heredoc wrappers as plain text and still parses the enclosed patch", () => {
    const wrappedPatch = [
      "<<EOF",
      "*** Begin Patch",
      "*** Add File: hello.txt",
      "+hello world",
      "*** End Patch",
      "NOT_EOF",
    ].join("\n");

    expect(parsePatch(wrappedPatch)).toEqual([
      { type: "add", path: "hello.txt", contents: "hello world" },
    ]);
    expect(parsePatch(`prefix\n${wrappedPatch}`)).toEqual([
      { type: "add", path: "hello.txt", contents: "hello world" },
    ]);
  });
});

describe("applyUpdateChunks error paths", () => {
  test("throws when change context does not match any line", () => {
    const chunks: UpdateFileChunk[] = [
      {
        change_context: "missing line",
        old_lines: ["beta"],
        new_lines: ["updated beta"],
      },
    ];

    expect(() => applyUpdateChunks("alpha\nbeta\n", "src/example.ts", chunks)).toThrow(
      "Failed to find context 'missing line' in src/example.ts",
    );
  });

  test("throws when old_lines do not exist in the file", () => {
    const chunks: UpdateFileChunk[] = [
      {
        old_lines: ["missing line"],
        new_lines: ["replacement line"],
      },
    ];

    expect(() => applyUpdateChunks("alpha\nbeta\n", "src/example.ts", chunks)).toThrow(
      "Failed to find expected lines in src/example.ts:\nmissing line",
    );
  });

  test("error includes 'already applied' hint when the new_lines are already in the file", () => {
    // Simulates an agent retrying apply_patch after partially applying earlier:
    // the old line is gone (already replaced), the new line is already present.
    const chunks: UpdateFileChunk[] = [
      {
        old_lines: ["const mainQuota = await getFreshMainQuota(auth.access, storage)"],
        new_lines: ["const mainQuota = await getMainQuotaForRouting(auth.access, storage)"],
      },
    ];

    const fileWithRewriteAlreadyApplied =
      "alpha\nconst mainQuota = await getMainQuotaForRouting(auth.access, storage)\nbeta\n";

    expect(() =>
      applyUpdateChunks(fileWithRewriteAlreadyApplied, "src/example.ts", chunks),
    ).toThrow(/already appears in the file/);
  });

  test("error has no 'already applied' hint when both old and new lines are absent", () => {
    // Genuinely wrong patch — neither side matches the file.
    const chunks: UpdateFileChunk[] = [
      {
        old_lines: ["missing old line"],
        new_lines: ["missing new line"],
      },
    ];

    let message = "";
    try {
      applyUpdateChunks("unrelated content\n", "src/example.ts", chunks);
    } catch (e) {
      message = (e as Error).message;
    }
    expect(message).toContain("Failed to find expected lines");
    expect(message).not.toContain("already appears in the file");
  });
});
