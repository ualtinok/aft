use std::fs;
use std::path::PathBuf;

use aft::compress::builtin_filters::ALL;
use aft::compress::toml_filter::{apply_filter, parse_filter, FilterSource};

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/integration/fixtures/compress_filters")
        .join(name)
}

fn load_filter(name: &str) -> aft::compress::toml_filter::TomlFilter {
    let (_, content) = ALL
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("builtin filter {name} not registered"));
    parse_filter(name, content, FilterSource::Builtin).expect("parse builtin")
}

fn run_fixture(name: &str) {
    let dir = fixture_dir(name);
    let input = fs::read_to_string(dir.join("input.txt")).expect("input.txt");
    let expected = fs::read_to_string(dir.join("expected.txt")).expect("expected.txt");
    let filter = load_filter(name);
    let actual = apply_filter(&filter, &input);
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "fixture mismatch for {name}",
    );
}

#[test]
fn ansible_playbook_filter() {
    run_fixture("ansible-playbook");
}

#[test]
fn df_filter() {
    run_fixture("df");
}

#[test]
fn docker_filter() {
    run_fixture("docker");
}

#[test]
fn du_filter() {
    run_fixture("du");
}

#[test]
fn find_filter() {
    run_fixture("find");
}

#[test]
fn gh_filter() {
    run_fixture("gh");
}

#[test]
fn gradle_filter() {
    run_fixture("gradle");
}

#[test]
fn helm_filter() {
    run_fixture("helm");
}

#[test]
fn kubectl_filter() {
    run_fixture("kubectl");
}

#[test]
fn ls_filter() {
    run_fixture("ls");
}

#[test]
fn terraform_filter() {
    run_fixture("terraform");
}

#[test]
fn tree_filter() {
    run_fixture("tree");
}

#[test]
fn wc_filter() {
    run_fixture("wc");
}

#[test]
fn xcodebuild_filter() {
    run_fixture("xcodebuild");
}
