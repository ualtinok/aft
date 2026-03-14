use std::path::Path;

use serde::Serialize;

use crate::language::LanguageProvider;
use crate::protocol::{RawRequest, Response};
use crate::symbols::{Range, Symbol};

/// A single entry in the outline tree.
///
/// Top-level symbols have an empty `members` vec. Classes/structs contain
/// their methods and nested types in `members`, forming a recursive tree.
#[derive(Debug, Clone, Serialize)]
pub struct OutlineEntry {
    pub name: String,
    pub kind: String,
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub exported: bool,
    pub members: Vec<OutlineEntry>,
}

/// Handle an `outline` request.
///
/// Expects `file` in request params. Calls `list_symbols()` on the provider,
/// then builds a nested tree: methods with a `parent` appear only under their
/// parent entry, not duplicated at top level.
pub fn handle_outline(req: &RawRequest, provider: &dyn LanguageProvider) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "outline: missing required param 'file'",
            );
        }
    };

    let path = Path::new(file);
    if !path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("file not found: {}", file),
        );
    }

    let symbols = match provider.list_symbols(path) {
        Ok(s) => s,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    let entries = build_outline_tree(&symbols);

    Response::success(&req.id, serde_json::json!({ "entries": entries }))
}

/// Build a nested outline tree from a flat symbol list.
///
/// Strategy: two passes.
/// 1. Convert every symbol to an `OutlineEntry` and index by name.
/// 2. Walk children (parent.is_some()) and attach them under their parent.
///    For multi-level nesting (e.g. OuterClass.InnerClass.inner_method),
///    we use the `scope_chain` to walk the full parent path.
///
/// Symbols whose parent can't be found in the list are promoted to top level
/// (defensive — shouldn't happen with well-formed parser output).
fn build_outline_tree(symbols: &[Symbol]) -> Vec<OutlineEntry> {
    // Separate top-level and child symbols
    let mut top_level: Vec<OutlineEntry> = Vec::new();
    let mut children: Vec<&Symbol> = Vec::new();

    for sym in symbols {
        if sym.parent.is_none() {
            top_level.push(symbol_to_entry(sym));
        } else {
            children.push(sym);
        }
    }

    // Build a name→index map for top-level entries
    // For multi-level nesting, we need to find entries recursively
    for child in &children {
        let entry = symbol_to_entry(child);
        let scope = &child.scope_chain;

        if scope.is_empty() {
            // Shouldn't happen if parent.is_some(), but be defensive
            top_level.push(entry);
            continue;
        }

        // Walk the scope chain to find the correct parent container
        if !insert_at_scope(&mut top_level, scope, entry.clone()) {
            // Parent not found — promote to top level
            top_level.push(entry);
        }
    }

    top_level
}

/// Recursively walk scope_chain to insert an entry under the correct parent.
///
/// scope_chain = ["OuterClass", "InnerClass"] means:
///   find "OuterClass" at this level → find "InnerClass" in its members → insert there
fn insert_at_scope(
    entries: &mut Vec<OutlineEntry>,
    scope_chain: &[String],
    entry: OutlineEntry,
) -> bool {
    if scope_chain.is_empty() {
        return false;
    }

    let target_name = &scope_chain[0];
    for existing in entries.iter_mut() {
        if existing.name == *target_name {
            if scope_chain.len() == 1 {
                // This is the direct parent — insert here
                existing.members.push(entry);
                return true;
            } else {
                // Recurse deeper
                return insert_at_scope(&mut existing.members, &scope_chain[1..], entry);
            }
        }
    }

    false
}

fn symbol_to_entry(sym: &Symbol) -> OutlineEntry {
    OutlineEntry {
        name: sym.name.clone(),
        kind: serde_json::to_value(&sym.kind)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", sym.kind).to_lowercase()),
        range: sym.range.clone(),
        signature: sym.signature.clone(),
        exported: sym.exported,
        members: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SymbolKind;

    fn make_symbol(
        name: &str,
        kind: SymbolKind,
        parent: Option<&str>,
        scope_chain: Vec<&str>,
        exported: bool,
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            range: Range {
                start_line: 0,
                start_col: 0,
                end_line: 0,
                end_col: 0,
            },
            signature: None,
            scope_chain: scope_chain.into_iter().map(String::from).collect(),
            exported,
            parent: parent.map(String::from),
        }
    }

    #[test]
    fn flat_symbols_stay_flat() {
        let symbols = vec![
            make_symbol("greet", SymbolKind::Function, None, vec![], true),
            make_symbol("Config", SymbolKind::Interface, None, vec![], true),
        ];
        let tree = build_outline_tree(&symbols);
        assert_eq!(tree.len(), 2);
        assert!(tree[0].members.is_empty());
        assert!(tree[1].members.is_empty());
    }

    #[test]
    fn methods_nest_under_class() {
        let symbols = vec![
            make_symbol("UserService", SymbolKind::Class, None, vec![], true),
            make_symbol(
                "getUser",
                SymbolKind::Method,
                Some("UserService"),
                vec!["UserService"],
                false,
            ),
            make_symbol(
                "addUser",
                SymbolKind::Method,
                Some("UserService"),
                vec!["UserService"],
                false,
            ),
        ];
        let tree = build_outline_tree(&symbols);
        assert_eq!(tree.len(), 1, "methods should not appear at top level");
        assert_eq!(tree[0].name, "UserService");
        assert_eq!(tree[0].members.len(), 2);
        assert_eq!(tree[0].members[0].name, "getUser");
        assert_eq!(tree[0].members[1].name, "addUser");
    }

    #[test]
    fn methods_not_duplicated_at_top_level() {
        let symbols = vec![
            make_symbol("Foo", SymbolKind::Class, None, vec![], false),
            make_symbol(
                "bar",
                SymbolKind::Method,
                Some("Foo"),
                vec!["Foo"],
                false,
            ),
        ];
        let tree = build_outline_tree(&symbols);
        // "bar" must NOT appear at top level
        assert!(
            tree.iter().all(|e| e.name != "bar"),
            "method should not be at top level"
        );
        assert_eq!(tree[0].members.len(), 1);
    }

    #[test]
    fn multi_level_nesting_python() {
        // OuterClass → InnerClass → inner_method
        let symbols = vec![
            make_symbol("OuterClass", SymbolKind::Class, None, vec![], false),
            make_symbol(
                "InnerClass",
                SymbolKind::Class,
                Some("OuterClass"),
                vec!["OuterClass"],
                false,
            ),
            make_symbol(
                "inner_method",
                SymbolKind::Method,
                Some("InnerClass"),
                vec!["OuterClass", "InnerClass"],
                false,
            ),
            make_symbol(
                "outer_method",
                SymbolKind::Method,
                Some("OuterClass"),
                vec!["OuterClass"],
                false,
            ),
        ];
        let tree = build_outline_tree(&symbols);
        assert_eq!(tree.len(), 1, "only OuterClass at top level");

        let outer = &tree[0];
        assert_eq!(outer.name, "OuterClass");
        assert_eq!(outer.members.len(), 2, "InnerClass + outer_method");

        let inner = outer.members.iter().find(|m| m.name == "InnerClass").unwrap();
        assert_eq!(inner.members.len(), 1);
        assert_eq!(inner.members[0].name, "inner_method");
    }

    #[test]
    fn all_symbol_kinds_handled() {
        let symbols = vec![
            make_symbol("f", SymbolKind::Function, None, vec![], false),
            make_symbol("C", SymbolKind::Class, None, vec![], false),
            make_symbol("m", SymbolKind::Method, Some("C"), vec!["C"], false),
            make_symbol("S", SymbolKind::Struct, None, vec![], false),
            make_symbol("I", SymbolKind::Interface, None, vec![], false),
            make_symbol("E", SymbolKind::Enum, None, vec![], false),
            make_symbol("T", SymbolKind::TypeAlias, None, vec![], false),
        ];
        let tree = build_outline_tree(&symbols);

        // 6 top-level (method is nested under class)
        assert_eq!(tree.len(), 6);

        let kinds: Vec<&str> = tree.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&"function"));
        assert!(kinds.contains(&"class"));
        assert!(kinds.contains(&"struct"));
        assert!(kinds.contains(&"interface"));
        assert!(kinds.contains(&"enum"));
        assert!(kinds.contains(&"type_alias"));

        // Method under class
        let class_entry = tree.iter().find(|e| e.name == "C").unwrap();
        assert_eq!(class_entry.members.len(), 1);
        assert_eq!(class_entry.members[0].kind, "method");
    }

    #[test]
    fn exported_flag_preserved() {
        let symbols = vec![
            make_symbol("exported_fn", SymbolKind::Function, None, vec![], true),
            make_symbol("internal_fn", SymbolKind::Function, None, vec![], false),
        ];
        let tree = build_outline_tree(&symbols);
        let exported = tree.iter().find(|e| e.name == "exported_fn").unwrap();
        let internal = tree.iter().find(|e| e.name == "internal_fn").unwrap();
        assert!(exported.exported);
        assert!(!internal.exported);
    }

    #[test]
    fn orphan_child_promoted_to_top_level() {
        // A method whose parent doesn't exist in the list
        let symbols = vec![make_symbol(
            "orphan",
            SymbolKind::Method,
            Some("MissingParent"),
            vec!["MissingParent"],
            false,
        )];
        let tree = build_outline_tree(&symbols);
        assert_eq!(tree.len(), 1, "orphan should be promoted to top level");
        assert_eq!(tree[0].name, "orphan");
    }
}
