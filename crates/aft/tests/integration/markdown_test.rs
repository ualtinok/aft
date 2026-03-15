use crate::helpers::AftProcess;
use std::fs;
use tempfile::TempDir;

const SAMPLE_MD: &str = r#"# Project Title

Some introduction text.

## Features

- Feature one
- Feature two

### Sub-feature A

Details about sub-feature A.

### Sub-feature B

Details about sub-feature B.

## Architecture

The architecture section.

### Component X

Info about X.

# Appendix

Final notes.
"#;

#[test]
fn markdown_outline_extracts_headings() {
    let dir = TempDir::new().unwrap();
    let md_file = dir.path().join("readme.md");
    fs::write(&md_file, SAMPLE_MD).unwrap();

    let mut aft = AftProcess::spawn();
    aft.configure(dir.path());
    let resp = aft.send(&format!(
        r#"{{"id":"md-1","command":"outline","file":"{}"}}"#,
        md_file.display()
    ));

    assert_eq!(resp["ok"], true, "outline should succeed: {:?}", resp);
    let entries = resp["entries"].as_array().expect("entries should be array");

    // Top-level: 2 h1 sections ("Project Title" and "Appendix")
    assert_eq!(
        entries.len(),
        2,
        "expected 2 top-level headings, got: {}",
        entries.len()
    );

    // Check first h1
    assert_eq!(entries[0]["name"], "Project Title");
    assert_eq!(entries[0]["kind"], "heading");
    assert_eq!(entries[0]["signature"], "# Project Title");

    // Check nested h2s under "Project Title"
    let members = entries[0]["members"].as_array().unwrap();
    assert_eq!(members.len(), 2, "Project Title should have 2 h2 children");
    assert_eq!(members[0]["name"], "Features");
    assert_eq!(members[0]["signature"], "## Features");
    assert_eq!(members[1]["name"], "Architecture");

    // Check nested h3s under "Features"
    let sub_members = members[0]["members"].as_array().unwrap();
    assert_eq!(sub_members.len(), 2, "Features should have 2 h3 children");
    assert_eq!(sub_members[0]["name"], "Sub-feature A");
    assert_eq!(sub_members[0]["signature"], "### Sub-feature A");
    assert_eq!(sub_members[1]["name"], "Sub-feature B");

    // Check second h1
    assert_eq!(entries[1]["name"], "Appendix");
    assert_eq!(entries[1]["signature"], "# Appendix");
    assert!(entries[1]["members"].as_array().unwrap().is_empty());
}

#[test]
fn markdown_outline_section_ranges_cover_content() {
    let dir = TempDir::new().unwrap();
    let md_file = dir.path().join("doc.md");
    fs::write(&md_file, SAMPLE_MD).unwrap();

    let mut aft = AftProcess::spawn();
    aft.configure(dir.path());
    let resp = aft.send(&format!(
        r#"{{"id":"md-2","command":"outline","file":"{}"}}"#,
        md_file.display()
    ));

    assert_eq!(resp["ok"], true);
    let entries = resp["entries"].as_array().unwrap();

    // "Features" is a child of the first h1
    let features = &entries[0]["members"].as_array().unwrap()[0];
    assert_eq!(features["name"], "Features");
    let range = &features["range"];
    let start = range["start_line"].as_u64().unwrap();
    let end = range["end_line"].as_u64().unwrap();

    // Section should cover multiple lines (heading + list + sub-sections)
    assert!(
        end > start + 3,
        "Features section should span multiple lines: {}-{}",
        start,
        end
    );
}

#[test]
fn markdown_zoom_by_heading_name() {
    let dir = TempDir::new().unwrap();
    let md_file = dir.path().join("readme.md");
    fs::write(&md_file, SAMPLE_MD).unwrap();

    let mut aft = AftProcess::spawn();
    aft.configure(dir.path());
    let resp = aft.send(&format!(
        r#"{{"id":"md-3","command":"zoom","file":"{}","symbol":"Architecture"}}"#,
        md_file.display()
    ));

    assert_eq!(
        resp["ok"], true,
        "zoom by heading name should work: {:?}",
        resp
    );
    let content = resp["content"].as_str().expect("content should be string");

    // Should contain the heading and its content
    assert!(
        content.contains("## Architecture"),
        "content should include heading"
    );
    assert!(
        content.contains("The architecture section"),
        "content should include body text"
    );
    assert!(
        content.contains("### Component X"),
        "content should include sub-heading"
    );
}

#[test]
fn markdown_zoom_line_range() {
    let dir = TempDir::new().unwrap();
    let md_file = dir.path().join("readme.md");
    fs::write(&md_file, SAMPLE_MD).unwrap();

    let mut aft = AftProcess::spawn();
    aft.configure(dir.path());
    let resp = aft.send(&format!(
        r#"{{"id":"md-4","command":"zoom","file":"{}","start_line":1,"end_line":3}}"#,
        md_file.display()
    ));

    assert_eq!(
        resp["ok"], true,
        "zoom by line range should work: {:?}",
        resp
    );
    let content = resp["content"].as_str().unwrap();
    assert!(
        content.contains("# Project Title"),
        "should contain first heading"
    );
}

#[test]
fn markdown_write_preserves_content() {
    let dir = TempDir::new().unwrap();
    let md_file = dir.path().join("new.md");

    let mut aft = AftProcess::spawn();
    aft.configure(dir.path());

    let new_content = "# New Doc\\n\\nSome content.\\n";
    let resp = aft.send(&format!(
        r#"{{"id":"md-5","command":"write","file":"{}","content":"{}"}}"#,
        md_file.display(),
        new_content
    ));

    assert_eq!(resp["ok"], true, "write should succeed: {:?}", resp);
    let on_disk = fs::read_to_string(&md_file).unwrap();
    assert!(
        on_disk.contains("# New Doc"),
        "written content should be on disk"
    );
}

#[test]
fn markdown_mdx_extension_supported() {
    let dir = TempDir::new().unwrap();
    let mdx_file = dir.path().join("doc.mdx");
    fs::write(&mdx_file, "# MDX Doc\n\nSome content.\n").unwrap();

    let mut aft = AftProcess::spawn();
    aft.configure(dir.path());
    let resp = aft.send(&format!(
        r#"{{"id":"md-6","command":"outline","file":"{}"}}"#,
        mdx_file.display()
    ));

    assert_eq!(resp["ok"], true, "mdx should be supported: {:?}", resp);
    let entries = resp["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "MDX Doc");
}
