/**
 * Maps AFT LSP server kinds to their npm packages and the binary names
 * exposed in `node_modules/.bin/` after install.
 *
 * Used by the auto-install flow to decide which packages to fetch and to
 * compute the `lsp_paths_extra` directory list passed to the Rust binary.
 *
 * Coverage matches OpenCode's `lsp/server.ts` `Npm.which(...)` calls. Any
 * server NOT in this table is either:
 *   1. Pattern A (PATH-only, install via system package manager)
 *   2. Pattern C (GitHub-release auto-download — see lsp-github-table.ts)
 *   3. Special (eslint/prisma — uses project's own node_modules)
 */

export interface NpmServerSpec {
  /** AFT server-kind id (matches `crates/aft/src/lsp/registry.rs::ServerKind::id_str`). */
  readonly id: string;
  /** npm package name. */
  readonly npm: string;
  /** Binary name installed under `node_modules/.bin/`. */
  readonly binary: string;
  /** File extensions (without dot) the LSP serves. Used for project-relevance discovery. */
  readonly extensions: readonly string[];
  /** Project-root marker files (presence triggers install). Optional. */
  readonly rootMarkers?: readonly string[];
}

export const NPM_LSP_TABLE: readonly NpmServerSpec[] = [
  {
    id: "typescript",
    npm: "typescript-language-server",
    binary: "typescript-language-server",
    extensions: ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"],
    rootMarkers: ["tsconfig.json", "jsconfig.json", "package.json"],
  },
  {
    id: "python",
    npm: "pyright",
    binary: "pyright-langserver",
    extensions: ["py", "pyi"],
    rootMarkers: ["pyproject.toml", "pyrightconfig.json", "requirements.txt"],
  },
  {
    id: "yaml",
    npm: "yaml-language-server",
    binary: "yaml-language-server",
    extensions: ["yaml", "yml"],
  },
  {
    id: "bash",
    npm: "bash-language-server",
    binary: "bash-language-server",
    extensions: ["sh", "bash", "zsh"],
  },
  {
    id: "dockerfile",
    npm: "dockerfile-language-server-nodejs",
    binary: "docker-langserver",
    // OpenCode also matches the literal filename "Dockerfile". AFT's
    // extension-only matcher catches `.dockerfile` — see registry.rs comment.
    extensions: ["dockerfile"],
    rootMarkers: ["Dockerfile", "dockerfile"],
  },
  {
    id: "vue",
    npm: "@vue/language-server",
    binary: "vue-language-server",
    extensions: ["vue"],
  },
  {
    id: "astro",
    npm: "@astrojs/language-server",
    binary: "astro-ls",
    extensions: ["astro"],
  },
  {
    id: "svelte",
    npm: "svelte-language-server",
    binary: "svelteserver",
    extensions: ["svelte"],
  },
  {
    id: "php-intelephense",
    npm: "intelephense",
    binary: "intelephense",
    extensions: ["php"],
  },
  {
    id: "biome",
    npm: "@biomejs/biome",
    binary: "biome",
    // Biome can run as LSP for the JS/TS family + json/jsonc.
    extensions: ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "json", "jsonc"],
    rootMarkers: ["biome.json", "biome.jsonc"],
  },
];

/** Find an entry by AFT server id. */
export function findNpmServerById(id: string): NpmServerSpec | undefined {
  return NPM_LSP_TABLE.find((entry) => entry.id === id);
}

/** Find an entry by binary name. */
export function findNpmServerByBinary(binary: string): NpmServerSpec | undefined {
  return NPM_LSP_TABLE.find((entry) => entry.binary === binary);
}
