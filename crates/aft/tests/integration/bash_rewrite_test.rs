use std::fs;
use std::sync::{Mutex, Once, OnceLock};

use aft::bash_rewrite::{parser, try_rewrite};
use aft::commands::edit_match::handle_edit_match;
use aft::config::Config;
use aft::context::AppContext;
use aft::parser::TreeSitterProvider;
use aft::protocol::RawRequest;
use log::{Level, LevelFilter, Log, Metadata, Record};
use serde_json::{json, Value};

static TEST_LOGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static LOGGER_INIT: Once = Once::new();

struct TestLogger;

impl Log for TestLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Warn
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            TEST_LOGS
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .expect("lock test logs")
                .push(format!("{}", record.args()));
        }
    }

    fn flush(&self) {}
}

fn init_test_logger() {
    LOGGER_INIT.call_once(|| {
        log::set_boxed_logger(Box::new(TestLogger)).expect("install test logger");
        log::set_max_level(LevelFilter::Warn);
    });
    TEST_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("lock test logs")
        .clear();
}

fn take_logs() -> Vec<String> {
    std::mem::take(
        &mut *TEST_LOGS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("lock test logs"),
    )
}

fn context(root: &std::path::Path, enabled: bool) -> AppContext {
    AppContext::new(
        Box::new(TreeSitterProvider::new()),
        Config {
            project_root: Some(root.to_path_buf()),
            experimental_bash_rewrite: enabled,
            restrict_to_project_root: true,
            ..Config::default()
        },
    )
}

fn request(command: &str, params: Value) -> RawRequest {
    RawRequest {
        id: "test".to_string(),
        command: command.to_string(),
        lsp_hints: None,
        session_id: None,
        params,
    }
}

fn rewrite(command: &str, ctx: &AppContext) -> Option<Value> {
    try_rewrite(command, ctx).map(|response| response.data)
}

fn output(data: &Value) -> &str {
    data.get("output")
        .and_then(Value::as_str)
        .expect("rewrite output")
}

fn assert_rewritten(command: &str, ctx: &AppContext, tool: &str) -> Value {
    let data = rewrite(command, ctx).unwrap_or_else(|| panic!("{command} should rewrite"));
    assert!(
        output(&data).contains(&format!("call the `{tool}` tool directly next time")),
        "missing footer: {data:?}"
    );
    data
}

#[test]
fn rewrites_grep_and_rejects_pipes() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "fn Needle() {}\n").unwrap();
    let ctx = context(dir.path(), true);

    let data = assert_rewritten(
        &format!("grep -ni needle {}", dir.path().join("src").display()),
        &ctx,
        "grep",
    );
    assert_eq!(data["success"], Value::Null);
    assert!(output(&data).contains("Needle"));
    assert!(rewrite("grep needle src | wc -l", &ctx).is_none());
    assert!(rewrite("grep -x needle src", &ctx).is_none());
}

#[test]
fn rewrites_rg_and_rejects_chains() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("notes.txt"), "alpha beta\n").unwrap();
    let ctx = context(dir.path(), true);

    let data = assert_rewritten(&format!("rg alpha {}", dir.path().display()), &ctx, "grep");
    assert!(output(&data).contains("alpha beta"));
    assert!(rewrite("rg alpha . && echo done", &ctx).is_none());
}

#[test]
fn rewrites_find_and_rejects_other_flags() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let ctx = context(dir.path(), true);

    let data = assert_rewritten("find src -name '*.rs' -type f", &ctx, "glob");
    assert!(output(&data).contains("src/main.rs"));
    assert!(rewrite("find src -maxdepth 1 -name '*.rs'", &ctx).is_none());
}

#[test]
fn rewrites_cat_read_and_rejects_multiple_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    fs::write(dir.path().join("b.txt"), "world\n").unwrap();
    let ctx = context(dir.path(), true);

    let data = assert_rewritten(
        &format!("cat {}", dir.path().join("a.txt").display()),
        &ctx,
        "read",
    );
    assert!(output(&data).contains("1: hello"));
    assert!(rewrite("cat a.txt b.txt", &ctx).is_none());
}

#[test]
fn rewrites_cat_append_and_echo_append() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), true);

    let notes = dir.path().join("notes.txt");
    assert_rewritten(
        &format!("cat >> {} <<EOF\nfirst\nEOF", notes.display()),
        &ctx,
        "edit",
    );
    assert_rewritten(
        &format!("echo \"second line\" >> {}", notes.display()),
        &ctx,
        "edit",
    );
    assert_eq!(fs::read_to_string(notes).unwrap(), "first\nsecond line\n");
    assert!(rewrite("cat > notes.txt", &ctx).is_none());
}

#[test]
fn rewrites_sed_range_and_rejects_other_forms() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("lines.txt"), "one\ntwo\nthree\n").unwrap();
    let ctx = context(dir.path(), true);

    let data = assert_rewritten(
        &format!("sed -n '2,3p' {}", dir.path().join("lines.txt").display()),
        &ctx,
        "read",
    );
    assert!(output(&data).contains("2: two"));
    assert!(output(&data).contains("3: three"));
    assert!(rewrite("sed 's/two/TWO/' lines.txt", &ctx).is_none());
}

#[test]
fn rewrites_ls_directory_and_rejects_unknown_flags() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "fn lib() {}\n").unwrap();
    let ctx = context(dir.path(), true);

    let data = assert_rewritten(
        &format!("ls -la {}", dir.path().join("src").display()),
        &ctx,
        "read",
    );
    assert!(output(&data).contains("lib.rs"));
    assert!(rewrite("ls -h src", &ctx).is_none());
}

#[test]
fn respects_experimental_flag() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    let ctx = context(dir.path(), false);

    assert!(rewrite("cat a.txt", &ctx).is_none());
}

/// Regression: when the rewrite target tool refuses (e.g. read returns
/// `path_not_found` for a file outside project_root), dispatch must fall
/// through to the actual bash command — the agent's intent was bash, the
/// rewrite is a transparent optimization. Returning the read error would
/// surprise the agent because bash itself has no project_root restriction.
#[test]
fn rewrite_target_failure_logs_warning_before_fallthrough() {
    init_test_logger();

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.txt");
    fs::write(&outside_path, "secret\n").unwrap();
    let ctx = context(dir.path(), true);

    assert!(
        rewrite(&format!("cat {}", outside_path.display()), &ctx).is_none(),
        "rewrite still falls through to bash when target tool refuses"
    );

    let logs = take_logs();
    assert!(
        logs.iter().any(|line| {
            line.contains("bash rewrite rule cat declined")
                && line.contains("read declined")
                && line.contains("outside the project root")
        }),
        "expected warn-level rewrite decline log, got {logs:?}"
    );
}

#[test]
fn rewrite_target_failure_falls_through_to_bash() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("outside.txt"), "secret\n").unwrap();
    let ctx = context(dir.path(), true);

    // cat a path outside project_root → read refuses → rewrite must NOT swallow
    // the error. try_rewrite returns None so the bash handler runs the actual
    // cat command.
    let outside_path = outside.path().join("outside.txt");
    assert!(
        rewrite(&format!("cat {}", outside_path.display()), &ctx).is_none(),
        "rewrite must fall through when read refuses outside-project paths"
    );

    // sed with the same outside path → read refuses → fall through.
    assert!(
        rewrite(&format!("sed -n '1,1p' {}", outside_path.display()), &ctx).is_none(),
        "sed→read fallthrough must apply for outside-project paths"
    );

    // ls of a directory outside project_root → read refuses → fall through.
    assert!(
        rewrite(&format!("ls {}", outside.path().display()), &ctx).is_none(),
        "ls→read fallthrough must apply for outside-project directories"
    );

    // grep against an outside path → grep refuses → fall through.
    assert!(
        rewrite(
            &format!("grep -n secret {}", outside.path().display()),
            &ctx
        )
        .is_none(),
        "grep fallthrough must apply for outside-project paths"
    );

    // Sanity: in-project rewrites still succeed (the helper isn't over-falling-through).
    fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    assert_rewritten(
        &format!("cat {}", dir.path().join("a.txt").display()),
        &ctx,
        "read",
    );
}

#[test]
fn parser_handles_quotes_escapes_heredocs_and_rejects_expansion() {
    let parsed = parser::parse("grep 'two words' \"src dir\"").expect("quoted parse");
    assert_eq!(parsed.args, vec!["grep", "two words", "src dir"]);

    let parsed = parser::parse(r"cat file\ name.txt").expect("escaped parse");
    assert_eq!(parsed.args, vec!["cat", "file name.txt"]);

    let parsed = parser::parse("cat >> out.txt <<EOF\nhello\nEOF").expect("heredoc parse");
    assert_eq!(parsed.args, vec!["cat"]);
    assert_eq!(parsed.appends_to.as_deref(), Some("out.txt"));
    assert_eq!(parsed.heredoc.as_deref(), Some("hello\n"));

    assert!(parser::parse("cat $(pwd)").is_none());
    assert!(parser::parse("cat `pwd`").is_none());
    assert!(parser::parse("echo $HOME").is_none());
}

#[test]
fn edit_append_op_appends_creates_and_reports_invalid_paths() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), false);
    let existing = dir.path().join("existing.txt");
    fs::write(&existing, "before\n").unwrap();

    let response = handle_edit_match(
        &request(
            "edit_match",
            json!({"op": "append", "file": existing.display().to_string(), "appendContent": "after\n"}),
        ),
        &ctx,
    );
    assert!(response.success, "append should succeed: {response:?}");
    assert_eq!(fs::read_to_string(&existing).unwrap(), "before\nafter\n");

    let response = handle_edit_match(
        &request(
            "edit_match",
            json!({"op": "append", "file": dir.path().join("new.txt").display().to_string(), "appendContent": "created\n"}),
        ),
        &ctx,
    );
    assert!(
        response.success,
        "create append should succeed: {response:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "created\n"
    );

    let response = handle_edit_match(
        &request(
            "edit_match",
            json!({"op": "append", "file": dir.path().join("missing/child.txt").display().to_string(), "appendContent": "nope", "createDirs": false}),
        ),
        &ctx,
    );
    assert!(!response.success, "invalid path should fail: {response:?}");
}
