import { type Dirent, existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

const MAX_WALK_DIRS = 200;
const MAX_WALK_DEPTH = 4;

const NOISE_DIRS = new Set([
  ".git",
  ".next",
  ".venv",
  "__pycache__",
  "build",
  "dist",
  "node_modules",
  "target",
]);

export function hasRootMarker(projectRoot: string, rootMarkers?: readonly string[]): boolean {
  if (!rootMarkers) return false;
  for (const marker of rootMarkers) {
    if (existsSync(join(projectRoot, marker))) return true;
  }
  return false;
}

/**
 * Bounded extension scan for project relevance decisions.
 *
 * Root-marker checks happen before callers use this helper. This walk only
 * answers "does this project contain one of the extensions we know how to
 * serve?" and deliberately skips common dependency/build/cache directories so
 * vendored files do not trigger heavyweight LSP installs.
 */
export function relevantExtensionsInProject(
  projectRoot: string,
  extToServer: Readonly<Record<string, readonly string[]>>,
): Set<string> {
  const wanted = new Set(Object.keys(extToServer).map((ext) => ext.toLowerCase()));
  const found = new Set<string>();
  if (wanted.size === 0) return found;

  const queue: Array<{ dir: string; depth: number }> = [{ dir: projectRoot, depth: 0 }];
  let visitedDirs = 0;

  while (queue.length > 0 && visitedDirs < MAX_WALK_DIRS) {
    const current = queue.shift();
    if (!current) break;
    visitedDirs += 1;

    let entries: Dirent[];
    try {
      entries = readdirSync(current.dir, { withFileTypes: true });
    } catch {
      continue;
    }

    for (const entry of entries) {
      if (entry.isDirectory()) {
        if (current.depth < MAX_WALK_DEPTH && !NOISE_DIRS.has(entry.name.toLowerCase())) {
          queue.push({ dir: join(current.dir, entry.name), depth: current.depth + 1 });
        }
        continue;
      }

      if (!entry.isFile()) continue;
      const ext = extensionOf(entry.name);
      if (ext && wanted.has(ext)) found.add(ext);
    }
  }

  return found;
}

function extensionOf(fileName: string): string | null {
  const dot = fileName.lastIndexOf(".");
  if (dot < 0 || dot === fileName.length - 1) return null;
  return fileName.slice(dot + 1).toLowerCase();
}
