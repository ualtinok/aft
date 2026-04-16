use std::fs;
use std::path::PathBuf;

use aft::semantic_index::SemanticIndex;

fn write_source_fixture(project_root: &std::path::Path) -> PathBuf {
    let source_file = project_root.join("src/lib.rs");
    fs::create_dir_all(source_file.parent().expect("source parent")).expect("create src dir");
    fs::write(
        &source_file,
        "pub fn handle_request(token: &str) -> bool {\n    !token.is_empty()\n}\n\npub fn normalize_user_id(input: &str) -> String {\n    input.trim().to_lowercase()\n}\n",
    )
    .expect("write source file");
    source_file
}

#[test]
fn build_returns_backend_http_errors_verbatim() {
    let project = tempfile::tempdir().expect("create project dir");
    let source_file = write_source_fixture(project.path());
    let files = vec![source_file];
    let mut embed = |_texts: Vec<String>| {
        Err::<Vec<Vec<f32>>, String>(
            "openai compatible request failed (HTTP 401): Unauthorized".to_string(),
        )
    };

    let error = match SemanticIndex::build(project.path(), &files, &mut embed, 16) {
        Err(error) => error,
        Ok(_) => panic!("expected backend HTTP error"),
    };

    assert_eq!(
        error,
        "openai compatible request failed (HTTP 401): Unauthorized"
    );
}

#[test]
fn build_returns_error_when_embedding_backend_returns_no_vectors() {
    let project = tempfile::tempdir().expect("create project dir");
    let source_file = write_source_fixture(project.path());
    let files = vec![source_file];
    let mut embed = |_texts: Vec<String>| Ok::<Vec<Vec<f32>>, String>(vec![]);

    let error = match SemanticIndex::build(project.path(), &files, &mut embed, 16) {
        Err(error) => error,
        Ok(_) => panic!("expected empty-vector validation error"),
    };

    assert_eq!(error, "embedding backend returned no vectors for 2 inputs");
}

#[test]
fn build_returns_error_when_embedding_dimension_changes_across_batches() {
    let project = tempfile::tempdir().expect("create project dir");
    let source_file = write_source_fixture(project.path());
    let files = vec![source_file];
    let mut call_count = 0usize;
    let mut embed = |_texts: Vec<String>| {
        call_count += 1;
        match call_count {
            1 => Ok::<Vec<Vec<f32>>, String>(vec![vec![1.0; 384]]),
            2 => Ok::<Vec<Vec<f32>>, String>(vec![vec![1.0; 512]]),
            _ => panic!("unexpected extra embedding batch"),
        }
    };

    let error = match SemanticIndex::build(project.path(), &files, &mut embed, 1) {
        Err(error) => error,
        Ok(_) => panic!("expected dimension mismatch validation error"),
    };

    assert_eq!(
        error,
        "embedding dimension changed across batches: expected 384, got 512"
    );
}

#[test]
fn build_returns_error_when_embedding_backend_returns_too_few_vectors() {
    let project = tempfile::tempdir().expect("create project dir");
    let source_file = write_source_fixture(project.path());

    let files = vec![source_file];
    let mut embed = |texts: Vec<String>| {
        Ok::<Vec<Vec<f32>>, String>(
            texts
                .into_iter()
                .skip(1)
                .map(|_| vec![1.0, 0.0, 0.0])
                .collect(),
        )
    };

    let error = match SemanticIndex::build(project.path(), &files, &mut embed, 16) {
        Err(error) => error,
        Ok(_) => panic!("expected vector count validation error"),
    };

    assert_eq!(error, "embedding backend returned 1 vectors for 2 inputs");
}
