use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};

use aft::semantic_index::{SemanticIndex, SemanticIndexFingerprint};
use log::{Level, LevelFilter, Log, Metadata, Record};

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
    clear_logs();
}

fn clear_logs() {
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

fn build_test_index(project_root: &Path) -> (SemanticIndex, PathBuf) {
    let source_file = project_root.join("src/lib.rs");
    fs::create_dir_all(source_file.parent().expect("source parent")).expect("create src dir");
    fs::write(
        &source_file,
        "pub fn handle_request(token: &str) -> bool {\n    !token.is_empty()\n}\n\npub fn normalize_user_id(input: &str) -> String {\n    input.trim().to_lowercase()\n}\n",
    )
    .expect("write source file");

    let files = vec![source_file.clone()];
    let mut embed = |texts: Vec<String>| {
        Ok::<Vec<Vec<f32>>, String>(
            texts
                .into_iter()
                .map(|text| {
                    if text.contains("handle_request") {
                        vec![1.0, 0.0, 0.0, 0.0]
                    } else if text.contains("normalize_user_id") {
                        vec![0.0, 1.0, 0.0, 0.0]
                    } else {
                        vec![0.0, 0.0, 1.0, 0.0]
                    }
                })
                .collect(),
        )
    };

    let index = SemanticIndex::build(project_root, &files, &mut embed, 16)
        .expect("build semantic index with stub embeddings");

    (index, source_file)
}

fn push_string(buf: &mut Vec<u8>, value: &str) {
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
}

fn build_v1_index_bytes(file: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    let file_str = file.to_string_lossy();

    bytes.push(1u8);
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());

    bytes.extend_from_slice(&1u32.to_le_bytes());
    push_string(&mut bytes, &file_str);
    bytes.extend_from_slice(&0u64.to_le_bytes());

    push_string(&mut bytes, &file_str);
    push_string(&mut bytes, "legacy_symbol");
    bytes.push(0u8);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.push(1u8);
    push_string(&mut bytes, "fn legacy_symbol() {}");
    push_string(
        &mut bytes,
        "file:src/lib.rs kind:function name:legacy_symbol",
    );
    for value in [0.1f32, 0.2, 0.3] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    bytes
}

#[test]
fn write_and_read_roundtrip_preserves_semantic_entries() {
    let project = tempfile::tempdir().expect("create project dir");
    let storage = tempfile::tempdir().expect("create storage dir");
    let (index, source_file) = build_test_index(project.path());

    index.write_to_disk(storage.path(), "roundtrip-project");

    let restored = SemanticIndex::read_from_disk(storage.path(), "roundtrip-project", None)
        .expect("restore semantic index from disk");

    assert_eq!(restored.len(), index.len());
    assert_eq!(restored.dimension(), index.dimension());

    let request_results = restored.search(&[1.0, 0.0, 0.0, 0.0], 2);
    assert!(!request_results.is_empty());
    assert_eq!(request_results[0].name, "handle_request");
    assert_eq!(request_results[0].file, source_file);
    assert!(request_results[0].snippet.contains("handle_request"));

    let normalize_results = restored.search(&[0.0, 1.0, 0.0, 0.0], 2);
    assert!(!normalize_results.is_empty());
    assert_eq!(normalize_results[0].name, "normalize_user_id");
}

#[test]
fn read_from_nonexistent_path_returns_none() {
    let storage = tempfile::tempdir().expect("create storage dir");

    let restored = SemanticIndex::read_from_disk(storage.path(), "missing-project", None);

    assert!(restored.is_none());
}

#[test]
fn read_from_corrupt_file_returns_none_and_logs_warning() {
    init_test_logger();

    let storage = tempfile::tempdir().expect("create storage dir");
    let semantic_dir = storage.path().join("semantic").join("corrupt-project");
    fs::create_dir_all(&semantic_dir).expect("create semantic dir");
    let semantic_file = semantic_dir.join("semantic.bin");
    fs::write(&semantic_file, b"corrupt").expect("write corrupt semantic file");

    let restored = SemanticIndex::read_from_disk(storage.path(), "corrupt-project", None);

    assert!(restored.is_none());
    assert!(
        !semantic_file.exists(),
        "corrupt semantic file should be removed after read failure"
    );

    let logs = take_logs();
    assert!(
        logs.iter()
            .any(|line| line.contains("corrupt semantic index")),
        "expected corrupt-index warning, got {logs:?}"
    );
}

#[test]
fn count_stale_files_marks_deleted_files_as_stale() {
    let project = tempfile::tempdir().expect("create project dir");
    let storage = tempfile::tempdir().expect("create storage dir");
    let (index, source_file) = build_test_index(project.path());

    index.write_to_disk(storage.path(), "stale-project");
    fs::remove_file(&source_file).expect("remove indexed source file");

    let restored = SemanticIndex::read_from_disk(storage.path(), "stale-project", None)
        .expect("restore semantic index from disk");

    assert_eq!(restored.count_stale_files(), 1);
}

#[test]
fn read_from_disk_rebuilds_v1_cache_when_fingerprint_is_expected() {
    let storage = tempfile::tempdir().expect("create storage dir");
    let legacy_file = storage.path().join("src/lib.rs");
    fs::create_dir_all(legacy_file.parent().expect("legacy parent")).expect("create src dir");
    fs::write(&legacy_file, "pub fn legacy_symbol() {}\n").expect("write legacy source file");

    let v1_bytes = build_v1_index_bytes(&legacy_file);
    let restored = SemanticIndex::from_bytes(&v1_bytes).expect("parse v1 semantic index bytes");
    assert!(restored.fingerprint().is_none());

    let semantic_dir = storage.path().join("semantic").join("v1-project");
    fs::create_dir_all(&semantic_dir).expect("create semantic dir");
    let semantic_file = semantic_dir.join("semantic.bin");
    fs::write(&semantic_file, &v1_bytes).expect("write v1 semantic index file");

    let expected_fingerprint = SemanticIndexFingerprint {
        backend: "fastembed".to_string(),
        model: "all-MiniLM-L6-v2".to_string(),
        base_url: "none".to_string(),
        dimension: 3,
    }
    .as_string();

    assert!(SemanticIndex::read_from_disk(
        storage.path(),
        "v1-project",
        Some(&expected_fingerprint)
    )
    .is_none());
    assert!(
        !semantic_file.exists(),
        "legacy semantic cache should be deleted after fingerprint mismatch"
    );
}
