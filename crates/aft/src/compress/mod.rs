//! Output compression for hoisted bash. Phase 0 stub; Phase 1 Tracks A and E fill in.

pub mod bun;
pub mod cargo;
pub mod generic;
pub mod git;
pub mod npm;
pub mod pnpm;
pub mod pytest;
pub mod tsc;

use crate::context::AppContext;
use bun::BunCompressor;
use cargo::CargoCompressor;
use generic::{strip_ansi, GenericCompressor};
use git::GitCompressor;
use npm::NpmCompressor;
use pnpm::PnpmCompressor;
use pytest::PytestCompressor;
use tsc::TscCompressor;

/// A `Compressor` knows how to reduce one specific command's output to fewer
/// tokens while preserving the information the agent needs.
pub trait Compressor {
    /// Returns true if this compressor handles the given command head + args.
    /// Called after generic detection (ANSI strip, dedup) so this is per-command logic only.
    fn matches(&self, command: &str) -> bool;

    /// Compress the output. Original is left untouched if compression fails.
    fn compress(&self, command: &str, output: &str) -> String;
}

/// Top-level dispatch: try each registered compressor; fall back to generic.
pub fn compress(command: &str, output: String, ctx: &AppContext) -> String {
    if !ctx.config().experimental_bash_compress {
        return output;
    }

    let stripped = strip_ansi(&output);
    let compressors: [&dyn Compressor; 7] = [
        &GitCompressor,
        &CargoCompressor,
        &TscCompressor,
        &NpmCompressor,
        &BunCompressor,
        &PnpmCompressor,
        &PytestCompressor,
    ];
    for compressor in compressors {
        if compressor.matches(command) {
            return compressor.compress(command, &stripped);
        }
    }

    GenericCompressor.compress(command, &stripped)
}
