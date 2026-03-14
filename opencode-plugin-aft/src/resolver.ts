import { execSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

/**
 * Locate the `aft` binary by checking (in order):
 * 1. PATH lookup via `which aft`
 * 2. ~/.cargo/bin/aft (Rust cargo install location)
 * 3. Platform-specific npm package (reserved for S07)
 *
 * Returns the absolute path to the first binary found.
 * Throws a descriptive error with install instructions if none found.
 */
export function findBinary(): string {
  // 1. Check PATH
  try {
    const result = execSync("which aft", {
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
    }).trim();
    if (result) return result;
  } catch {
    // `which` exits non-zero if not found — continue
  }

  // 2. Check ~/.cargo/bin/aft
  const cargoPath = join(homedir(), ".cargo", "bin", "aft");
  if (existsSync(cargoPath)) return cargoPath;

  // 3. Platform-specific npm package (S07 — not yet implemented)
  // Future: resolve `@aft/${platform}-${arch}/bin/aft` from node_modules

  throw new Error(
    [
      "Could not find the `aft` binary.",
      "",
      "Install it using one of these methods:",
      "  cargo install aft          # from crates.io",
      "  cargo build --release      # from source (binary at target/release/aft)",
      "",
      "Or add the aft directory to your PATH.",
    ].join("\n"),
  );
}
