//! Output compression for hoisted bash.
//!
//! Compression has three tiers, tried in this order:
//!
//! 1. **Rust [`Compressor`] modules** — stateful, hand-written parsers for
//!    high-traffic tools where heuristics like JSON parsing or section
//!    detection are required. Always wins when matched.
//! 2. **TOML filters** — declarative strip + truncate + cap + shortcircuit
//!    rules for the long tail of CLI tools. Loaded from builtin / user /
//!    project sources via [`toml_filter::build_registry`]. See
//!    [`toml_filter`] and [`trust`] for the trust model.
//! 3. **[`generic`] fallback** — ANSI strip + consecutive-dedup +
//!    middle-truncate. Always applies when no Rust module or TOML filter
//!    matches.

pub mod biome;
pub mod builtin_filters;
pub mod bun;
pub mod cargo;
pub mod eslint;
pub mod generic;
pub mod git;
pub mod npm;
pub mod pnpm;
pub mod pytest;
pub mod toml_filter;
pub mod trust;
pub mod tsc;
pub mod vitest;

use crate::context::AppContext;
use biome::BiomeCompressor;
use bun::BunCompressor;
use cargo::CargoCompressor;
use eslint::EslintCompressor;
use generic::{strip_ansi, GenericCompressor};
use git::GitCompressor;
use npm::NpmCompressor;
use pnpm::PnpmCompressor;
use pytest::PytestCompressor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use toml_filter::{apply_filter, FilterRegistry};
use tsc::TscCompressor;
use vitest::VitestCompressor;

/// Thread-safe handle to the TOML filter registry. Shared between
/// `AppContext::filter_registry()` (for direct use in command handlers) and
/// `BgTaskRegistry`'s output compression closure (for use from the watchdog
/// thread).
pub type SharedFilterRegistry = Arc<RwLock<FilterRegistry>>;

/// A `Compressor` knows how to reduce one specific command's output to fewer
/// tokens while preserving the information the agent needs.
pub trait Compressor {
    /// Returns true if this compressor handles the given command head + args.
    /// Called after generic detection (ANSI strip, dedup) so this is per-command logic only.
    fn matches(&self, command: &str) -> bool;

    /// Compress the output. Original is left untouched if compression fails.
    fn compress(&self, command: &str, output: &str) -> String;
}

/// Top-level dispatch: try Rust modules, then TOML filters, then generic fallback.
///
/// Convenience wrapper for command handlers that already hold an `AppContext`.
/// Backs onto [`compress_with_registry`] which is thread-safe for use from the
/// `BgTaskRegistry` watchdog.
pub fn compress(command: &str, output: String, ctx: &AppContext) -> String {
    if !ctx.config().experimental_bash_compress {
        return output;
    }
    let registry_handle = ctx.shared_filter_registry();
    let guard = match registry_handle.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    compress_with_registry(command, &output, &guard)
}

/// Thread-safe dispatch that does not need `AppContext`. Caller is responsible
/// for the `experimental_bash_compress` gate (the registry has no opinion).
///
/// Used from background threads (notably the `BgTaskRegistry` watchdog and
/// completion-frame emitter) where lock-free access is required.
pub fn compress_with_registry(command: &str, output: &str, registry: &FilterRegistry) -> String {
    let stripped = strip_ansi(output);

    // Tier 1: Rust modules — always win when matched.
    let compressors: [&dyn Compressor; 10] = [
        &GitCompressor,
        &CargoCompressor,
        &TscCompressor,
        &NpmCompressor,
        &BunCompressor,
        &PnpmCompressor,
        &PytestCompressor,
        &EslintCompressor,
        &VitestCompressor,
        &BiomeCompressor,
    ];
    for compressor in compressors {
        if compressor.matches(command) {
            return compressor.compress(command, &stripped);
        }
    }

    // Tier 2: TOML filters.
    if let Some(filter) = registry.lookup(command) {
        return apply_filter(filter, &stripped);
    }

    // Tier 3: generic fallback.
    GenericCompressor.compress(command, &stripped)
}

/// Build the registry of TOML filters from the standard sources for the
/// active context. Called lazily by [`AppContext::filter_registry`].
///
/// Layering (highest priority first):
/// 1. Project filters at `<project_root>/.aft/filters/*.toml` — loaded only
///    when the project is in the trusted set (see [`trust`]).
/// 2. User filters at `<storage_dir>/filters/*.toml`.
/// 3. Builtin filters compiled into the binary via [`builtin_filters`].
pub fn build_registry_for_context(ctx: &AppContext) -> FilterRegistry {
    let config = ctx.config();
    let storage_dir = config.storage_dir.clone();
    let project_root = config.project_root.clone();
    drop(config);

    let user_dir = storage_dir.as_ref().map(|d| d.join("filters"));
    let project_dir = match (project_root.as_ref(), storage_dir.as_ref()) {
        (Some(root), Some(storage)) => {
            if trust::is_project_trusted(Some(storage), root) {
                Some(root.join(".aft").join("filters"))
            } else {
                None
            }
        }
        _ => None,
    };

    toml_filter::build_registry(
        builtin_filters::ALL,
        user_dir.as_deref(),
        project_dir.as_deref(),
    )
}

/// Resolve the user-filter directory for an arbitrary storage_dir. Used by
/// `aft doctor filters` to inspect filters without needing a live AppContext.
pub fn user_filter_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join("filters")
}

/// Resolve the project-filter directory for an arbitrary project root.
/// Returns the directory regardless of trust state — caller must check trust
/// separately if it wants to gate loading.
pub fn project_filter_dir(project_root: &Path) -> PathBuf {
    project_root.join(".aft").join("filters")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_and_project_filter_dir_helpers() {
        let storage = Path::new("/tmp/aft-storage");
        assert_eq!(
            user_filter_dir(storage),
            Path::new("/tmp/aft-storage/filters")
        );

        let project = Path::new("/repo");
        assert_eq!(project_filter_dir(project), Path::new("/repo/.aft/filters"));
    }
}
