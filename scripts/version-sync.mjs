#!/usr/bin/env node

/**
 * version-sync.mjs
 *
 * Synchronizes version across all AFT packages from a single source of truth.
 *
 * Usage:
 *   node scripts/version-sync.mjs 0.2.0           # set version to 0.2.0
 *   node scripts/version-sync.mjs --from-tag       # read from GITHUB_REF_NAME (e.g. v0.2.0)
 *   node scripts/version-sync.mjs 0.2.0 --dry-run  # preview changes without writing
 *
 * Updates 9 locations:
 *   1-5. npm/{platform}/package.json  → version field
 *   6.   aft-opencode/package.json → version field + all optionalDependencies versions
 *   7.   aft-pi/package.json → version field + all optionalDependencies versions
 *   8.   aft-cli/package.json → version field
 *   9.   Cargo.toml → version field
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");

const SEMVER_RE = /^\d+\.\d+\.\d+(?:-[\w.]+)?(?:\+[\w.]+)?$/;

const PLATFORM_DIRS = ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64"];

function parseArgs(argv) {
  const args = argv.slice(2);
  let version = null;
  let fromTag = false;
  let dryRun = false;

  for (const arg of args) {
    if (arg === "--from-tag") {
      fromTag = true;
    } else if (arg === "--dry-run") {
      dryRun = true;
    } else if (!version && !arg.startsWith("-")) {
      version = arg;
    } else {
      console.error(`Unknown argument: ${arg}`);
      process.exit(1);
    }
  }

  if (fromTag) {
    const ref = process.env.GITHUB_REF_NAME;
    if (!ref) {
      console.error("--from-tag requires GITHUB_REF_NAME environment variable");
      process.exit(1);
    }
    // Strip leading 'v' from tag (e.g. v0.2.0 → 0.2.0)
    version = ref.replace(/^v/, "");
  }

  if (!version) {
    console.error(
      "Usage: version-sync.mjs <version> [--dry-run]\n" +
        "       version-sync.mjs --from-tag [--dry-run]",
    );
    process.exit(1);
  }

  if (!SEMVER_RE.test(version)) {
    console.error(`Invalid semver version: '${version}'`);
    process.exit(1);
  }

  return { version, dryRun };
}

function updateJsonFile(filePath, version, updates, dryRun) {
  const content = readFileSync(filePath, "utf-8");
  const pkg = JSON.parse(content);
  const changes = [];

  if (pkg.version !== version) {
    changes.push(`  version: ${pkg.version} → ${version}`);
    pkg.version = version;
  }

  // Update optionalDependencies versions if requested
  if (updates?.optionalDependencies && pkg.optionalDependencies) {
    for (const [dep, oldVer] of Object.entries(pkg.optionalDependencies)) {
      if (oldVer !== version) {
        changes.push(`  optionalDependencies["${dep}"]: ${oldVer} → ${version}`);
        pkg.optionalDependencies[dep] = version;
      }
    }
  }

  if (changes.length === 0) {
    return { path: filePath, changes: ["  (already at target version)"] };
  }

  if (!dryRun) {
    writeFileSync(filePath, `${JSON.stringify(pkg, null, 2)}\n`, "utf-8");
  }

  return { path: filePath, changes };
}

function updateCargoToml(filePath, version, dryRun) {
  const content = readFileSync(filePath, "utf-8");
  const changes = [];

  // Match the version line under [package] — first version = line in [package] section
  const versionRe = /^(version\s*=\s*)"([^"]+)"/m;
  const match = content.match(versionRe);

  if (!match) {
    return { path: filePath, changes: ["  WARNING: could not find version field"] };
  }

  if (match[2] === version) {
    return { path: filePath, changes: ["  (already at target version)"] };
  }

  changes.push(`  version: ${match[2]} → ${version}`);

  if (!dryRun) {
    const updated = content.replace(versionRe, `$1"${version}"`);
    writeFileSync(filePath, updated, "utf-8");
  }

  return { path: filePath, changes };
}

// --- Main ---

const { version, dryRun } = parseArgs(process.argv);

console.log(`${dryRun ? "[DRY RUN] " : ""}Syncing version to ${version}\n`);

const results = [];

// 1-5: Platform packages
for (const dir of PLATFORM_DIRS) {
  const filePath = join(root, "packages", "npm", dir, "package.json");
  results.push(updateJsonFile(filePath, version, {}, dryRun));
}

// 6: @cortexkit/aft-opencode
const corePath = join(root, "packages", "opencode-plugin", "package.json");
results.push(updateJsonFile(corePath, version, { optionalDependencies: true }, dryRun));

// 7: @cortexkit/aft-pi
const piPath = join(root, "packages", "pi-plugin", "package.json");
results.push(updateJsonFile(piPath, version, { optionalDependencies: true }, dryRun));

// 8: @cortexkit/aft (unified CLI)
const cliPath = join(root, "packages", "aft-cli", "package.json");
results.push(updateJsonFile(cliPath, version, {}, dryRun));

// 9: Cargo.toml
const cargoPath = join(root, "crates", "aft", "Cargo.toml");
results.push(updateCargoToml(cargoPath, version, dryRun));

// Report
let updateCount = 0;
for (const { path, changes } of results) {
  const relativePath = path.replace(`${root}/`, "");
  console.log(`${relativePath}:`);
  for (const change of changes) {
    console.log(change);
    if (!change.includes("already at")) updateCount++;
  }
}

console.log(
  `\n${dryRun ? "[DRY RUN] " : ""}${updateCount} update(s) across ${results.length} files.`,
);
