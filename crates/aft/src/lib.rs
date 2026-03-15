pub mod ast_grep_lang;
pub mod backup;
pub mod callgraph;
pub mod calls;
pub mod checkpoint;
pub mod commands;
pub mod config;
pub mod context;
pub mod edit;
pub mod error;
pub mod extract;
pub mod format;
pub mod imports;
pub mod indent;
pub mod language;
pub mod lsp;
pub mod lsp_hints;
pub mod parser;
pub mod protocol;
pub mod symbols;

#[cfg(test)]
mod tests {
    use super::*;
    use config::Config;
    use error::AftError;
    use protocol::{RawRequest, Response};

    // --- Protocol serialization ---

    #[test]
    fn raw_request_deserializes_ping() {
        let json = r#"{"id":"1","command":"ping"}"#;
        let req: RawRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, "1");
        assert_eq!(req.command, "ping");
        assert!(req.lsp_hints.is_none());
    }

    #[test]
    fn raw_request_deserializes_echo_with_params() {
        let json = r#"{"id":"2","command":"echo","message":"hello"}"#;
        let req: RawRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, "2");
        assert_eq!(req.command, "echo");
        // "message" is captured in the flattened params
        assert_eq!(req.params["message"], "hello");
    }

    #[test]
    fn raw_request_preserves_unknown_fields() {
        let json = r#"{"id":"3","command":"ping","future_field":"abc","nested":{"x":1}}"#;
        let req: RawRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.params["future_field"], "abc");
        assert_eq!(req.params["nested"]["x"], 1);
    }

    #[test]
    fn raw_request_with_lsp_hints() {
        let json = r#"{"id":"4","command":"ping","lsp_hints":{"completions":["foo","bar"]}}"#;
        let req: RawRequest = serde_json::from_str(json).unwrap();
        assert!(req.lsp_hints.is_some());
        let hints = req.lsp_hints.unwrap();
        assert_eq!(hints["completions"][0], "foo");
    }

    #[test]
    fn response_success_round_trip() {
        let resp = Response::success("42", serde_json::json!({"command": "pong"}));
        let json_str = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["id"], "42");
        assert_eq!(v["ok"], true);
        assert_eq!(v["command"], "pong");
    }

    #[test]
    fn response_error_round_trip() {
        let resp = Response::error("99", "unknown_command", "unknown command: foo");
        let json_str = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["id"], "99");
        assert_eq!(v["ok"], false);
        assert_eq!(v["code"], "unknown_command");
        assert_eq!(v["message"], "unknown command: foo");
    }

    // --- Error formatting ---

    #[test]
    fn error_display_symbol_not_found() {
        let err = AftError::SymbolNotFound {
            name: "foo".into(),
            file: "bar.rs".into(),
        };
        assert_eq!(err.to_string(), "symbol 'foo' not found in bar.rs");
        assert_eq!(err.code(), "symbol_not_found");
    }

    #[test]
    fn error_display_ambiguous_symbol() {
        let err = AftError::AmbiguousSymbol {
            name: "Foo".into(),
            candidates: vec!["a.rs:10".into(), "b.rs:20".into()],
        };
        let s = err.to_string();
        assert!(s.contains("Foo"));
        assert!(s.contains("a.rs:10, b.rs:20"));
    }

    #[test]
    fn error_display_parse_error() {
        let err = AftError::ParseError {
            message: "unexpected token".into(),
        };
        assert_eq!(err.to_string(), "parse error: unexpected token");
    }

    #[test]
    fn error_display_file_not_found() {
        let err = AftError::FileNotFound {
            path: "/tmp/missing.rs".into(),
        };
        assert_eq!(err.to_string(), "file not found: /tmp/missing.rs");
    }

    #[test]
    fn error_display_invalid_request() {
        let err = AftError::InvalidRequest {
            message: "missing field".into(),
        };
        assert_eq!(err.to_string(), "invalid request: missing field");
    }

    #[test]
    fn error_display_checkpoint_not_found() {
        let err = AftError::CheckpointNotFound {
            name: "pre-refactor".into(),
        };
        assert_eq!(err.to_string(), "checkpoint not found: pre-refactor");
        assert_eq!(err.code(), "checkpoint_not_found");
    }

    #[test]
    fn error_display_no_undo_history() {
        let err = AftError::NoUndoHistory {
            path: "src/main.rs".into(),
        };
        assert_eq!(err.to_string(), "no undo history for: src/main.rs");
        assert_eq!(err.code(), "no_undo_history");
    }

    #[test]
    fn error_display_ambiguous_match() {
        let err = AftError::AmbiguousMatch {
            pattern: "TODO".into(),
            count: 5,
        };
        assert_eq!(
            err.to_string(),
            "pattern 'TODO' matches 5 occurrences, expected exactly 1"
        );
        assert_eq!(err.code(), "ambiguous_match");
    }

    #[test]
    fn error_to_json_has_code_and_message() {
        let err = AftError::FileNotFound { path: "/x".into() };
        let j = err.to_error_json();
        assert_eq!(j["code"], "file_not_found");
        assert!(j["message"].as_str().unwrap().contains("/x"));
    }

    // --- Config defaults ---

    #[test]
    fn config_default_values() {
        let cfg = Config::default();
        assert!(cfg.project_root.is_none());
        assert_eq!(cfg.validation_depth, 1);
        assert_eq!(cfg.checkpoint_ttl_hours, 24);
        assert_eq!(cfg.max_symbol_depth, 10);
        assert_eq!(cfg.formatter_timeout_secs, 10);
        assert_eq!(cfg.type_checker_timeout_secs, 30);
    }
}
