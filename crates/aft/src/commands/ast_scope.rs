use std::path::{Path, PathBuf};

use crate::ast_grep_lang::AstGrepLang;
use crate::protocol::Response;

pub(crate) struct AstScope {
    pub files: Vec<PathBuf>,
    pub scope_warnings: Vec<String>,
    pub no_files_matched_scope: bool,
}

struct SearchRoot {
    path: PathBuf,
    label: Option<String>,
}

pub(crate) fn collect_ast_files(
    req_id: &str,
    command: &str,
    project_root: &Path,
    lang: &AstGrepLang,
    paths: &[String],
    globs: &[String],
) -> Result<AstScope, Response> {
    let roots = resolve_roots(project_root, paths);

    for root in &roots {
        if !root.path.exists() {
            return Err(Response::error(
                req_id,
                "path_not_found",
                format!(
                    "{}: search path does not exist: {}",
                    command,
                    root.path.display()
                ),
            ));
        }
    }

    let files = walk_roots(project_root, &roots, lang, globs);
    let scope_warnings = scope_warnings(project_root, &roots, lang, globs);
    let no_files_matched_scope = files.is_empty();

    Ok(AstScope {
        files,
        scope_warnings,
        no_files_matched_scope,
    })
}

fn resolve_roots(project_root: &Path, paths: &[String]) -> Vec<SearchRoot> {
    if paths.is_empty() {
        return vec![SearchRoot {
            path: project_root.to_path_buf(),
            label: None,
        }];
    }

    paths
        .iter()
        .map(|path| {
            let candidate = PathBuf::from(path);
            let resolved = if candidate.is_absolute() {
                candidate
            } else {
                project_root.join(path)
            };

            SearchRoot {
                path: resolved,
                label: Some(path.clone()),
            }
        })
        .collect()
}

fn scope_warnings(
    project_root: &Path,
    roots: &[SearchRoot],
    lang: &AstGrepLang,
    globs: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();

    for root in roots.iter().filter(|root| root.label.is_some()) {
        if walk_root(project_root, root, lang, &[]).is_empty() {
            warnings.push(format!(
                "{} → no files",
                root.label.as_deref().expect("explicit root label")
            ));
        }
    }

    let exclude_globs: Vec<String> = globs
        .iter()
        .filter(|glob| glob.starts_with('!'))
        .cloned()
        .collect();

    for include_glob in globs.iter().filter(|glob| !glob.starts_with('!')) {
        let mut single_glob_scope = Vec::with_capacity(1 + exclude_globs.len());
        single_glob_scope.push(include_glob.clone());
        single_glob_scope.extend(exclude_globs.iter().cloned());

        if walk_roots(project_root, roots, lang, &single_glob_scope).is_empty() {
            warnings.push(format!("{} → no files", include_glob));
        }
    }

    warnings.sort();
    warnings.dedup();
    warnings
}

fn walk_roots(
    project_root: &Path,
    roots: &[SearchRoot],
    lang: &AstGrepLang,
    globs: &[String],
) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|root| walk_root(project_root, root, lang, globs))
        .collect()
}

fn walk_root(
    project_root: &Path,
    root: &SearchRoot,
    lang: &AstGrepLang,
    globs: &[String],
) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let filter_root = if root.path.starts_with(project_root) {
        project_root
    } else {
        root.path.as_path()
    };
    let overrides = build_overrides(filter_root, globs);

    let mut builder = WalkBuilder::new(&root.path);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                return !matches!(
                    name.as_ref(),
                    "node_modules"
                        | "target"
                        | "venv"
                        | ".venv"
                        | ".git"
                        | "__pycache__"
                        | ".tox"
                        | "dist"
                        | "build"
                );
            }
            true
        });

    if let Some(overrides) = overrides {
        builder.overrides(overrides);
    }

    builder
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| lang.matches_path(path))
        .collect()
}

fn build_overrides(root: &Path, globs: &[String]) -> Option<ignore::overrides::Override> {
    if globs.is_empty() {
        return None;
    }

    let mut override_builder = ignore::overrides::OverrideBuilder::new(root);
    for glob in globs {
        if let Some(exclude) = glob.strip_prefix('!') {
            let _ = override_builder.add(&format!("!{}", exclude));
        } else {
            let _ = override_builder.add(glob);
        }
    }

    override_builder.build().ok()
}
