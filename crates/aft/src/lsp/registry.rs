use std::path::Path;

/// Unique identifier for a language server kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerKind {
    TypeScript,
    Python,
    Rust,
    Go,
}

/// Definition of a language server.
#[derive(Debug, PartialEq, Eq)]
pub struct ServerDef {
    pub kind: ServerKind,
    /// Display name.
    pub name: &'static str,
    /// File extensions this server handles.
    pub extensions: &'static [&'static str],
    /// Binary name to look up on PATH.
    pub binary: &'static str,
    /// Arguments to pass when spawning.
    pub args: &'static [&'static str],
    /// Root marker files — presence indicates a workspace root.
    pub root_markers: &'static [&'static str],
}

const BUILTIN_SERVERS: &[ServerDef] = &[
    ServerDef {
        kind: ServerKind::TypeScript,
        name: "TypeScript Language Server",
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        binary: "typescript-language-server",
        args: &["--stdio"],
        root_markers: &["tsconfig.json", "jsconfig.json", "package.json"],
    },
    ServerDef {
        kind: ServerKind::Python,
        name: "Pyright",
        extensions: &["py", "pyi"],
        binary: "pyright-langserver",
        args: &["--stdio"],
        root_markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "pyrightconfig.json",
            "requirements.txt",
        ],
    },
    ServerDef {
        kind: ServerKind::Rust,
        name: "rust-analyzer",
        extensions: &["rs"],
        binary: "rust-analyzer",
        args: &[],
        root_markers: &["Cargo.toml"],
    },
    ServerDef {
        kind: ServerKind::Go,
        name: "gopls",
        extensions: &["go"],
        binary: "gopls",
        args: &["serve"],
        root_markers: &["go.mod"],
    },
];

/// Built-in server definitions.
pub fn builtin_servers() -> &'static [ServerDef] {
    BUILTIN_SERVERS
}

impl ServerDef {
    /// Check if this server handles a given file extension.
    pub fn matches_extension(&self, ext: &str) -> bool {
        self.extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
    }

    /// Check if the server binary is available on PATH.
    pub fn is_available(&self) -> bool {
        which::which(self.binary).is_ok()
    }
}

/// Find all server definitions that handle a given file path.
pub fn servers_for_file(path: &Path) -> Vec<&'static ServerDef> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();

    builtin_servers()
        .iter()
        .filter(|server| server.matches_extension(extension))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{servers_for_file, ServerKind};

    fn matching_kinds(path: &str) -> Vec<ServerKind> {
        servers_for_file(Path::new(path))
            .into_iter()
            .map(|server| server.kind)
            .collect()
    }

    #[test]
    fn test_servers_for_typescript_file() {
        assert_eq!(matching_kinds("/tmp/file.ts"), vec![ServerKind::TypeScript]);
    }

    #[test]
    fn test_servers_for_python_file() {
        assert_eq!(matching_kinds("/tmp/file.py"), vec![ServerKind::Python]);
    }

    #[test]
    fn test_servers_for_rust_file() {
        assert_eq!(matching_kinds("/tmp/file.rs"), vec![ServerKind::Rust]);
    }

    #[test]
    fn test_servers_for_go_file() {
        assert_eq!(matching_kinds("/tmp/file.go"), vec![ServerKind::Go]);
    }

    #[test]
    fn test_servers_for_unknown_file() {
        assert!(matching_kinds("/tmp/file.txt").is_empty());
    }

    #[test]
    fn test_tsx_matches_typescript() {
        assert_eq!(
            matching_kinds("/tmp/file.tsx"),
            vec![ServerKind::TypeScript]
        );
    }

    #[test]
    fn test_case_insensitive_extension() {
        assert_eq!(matching_kinds("/tmp/file.TS"), vec![ServerKind::TypeScript]);
    }
}
