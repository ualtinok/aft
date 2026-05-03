use aft::compress::bun::BunCompressor;
use aft::compress::npm::NpmCompressor;
use aft::compress::pnpm::PnpmCompressor;
use aft::compress::pytest::PytestCompressor;
use aft::compress::tsc::TscCompressor;
use aft::compress::{self, Compressor};
use aft::config::Config;
use aft::context::AppContext;
use aft::parser::TreeSitterProvider;

fn compress_context() -> AppContext {
    AppContext::new(
        Box::new(TreeSitterProvider::new()),
        Config {
            experimental_bash_compress: true,
            ..Config::default()
        },
    )
}

#[test]
fn npm_install_caps_deprecations_and_keeps_summary() {
    let mut output = String::new();
    for index in 0..8 {
        output.push_str(&format!(
            "npm WARN deprecated package-{index}@1.0.0: use replacement-{index}\n"
        ));
        output.push_str(&format!(
            "npm http fetch GET 200 https://registry.npmjs.org/package-{index} 12ms\n"
        ));
    }
    output.push_str("added 300 packages in 10s\n\n80 packages are looking for funding\n  run `npm fund` for details\n\naudited 301 packages in 11s\nfound 0 vulnerabilities\n");

    let compressed = NpmCompressor.compress("npm install", &output);
    assert!(compressed.contains("package-0@1.0.0"));
    assert!(compressed.contains("package-4@1.0.0"));
    assert!(compressed.contains("... and 3 more deprecation warnings"));
    assert!(!compressed.contains("package-7@1.0.0"));
    assert!(!compressed.contains("npm http fetch"));
    assert!(!compressed.contains("added 300 packages"));
    assert!(compressed.contains("audited 301 packages"));
    assert!(compressed.contains("found 0 vulnerabilities"));

    let ratio = compressed.len() as f32 / output.len() as f32;
    assert!(ratio < 0.70, "ratio was {ratio}");
}

#[test]
fn bun_install_drops_resolver_noise_but_keeps_errors_and_summary() {
    let mut output = String::new();
    for index in 0..30 {
        output.push_str(&format!("Resolving dependencies {index}/30\n"));
        output.push_str(&format!("Downloaded dep-{index}\n"));
    }
    output.push_str("error: GET https://registry.example/dep - 500\n42 packages installed [1234.00ms]\nSaved lockfile\n");

    let compressed = BunCompressor.compress("bun install", &output);
    assert!(!compressed.contains("Resolving dependencies"));
    assert!(!compressed.contains("Downloaded dep-"));
    assert!(compressed.contains("error: GET https://registry.example/dep - 500"));
    assert!(compressed.contains("42 packages installed"));
    assert!(compressed.contains("Saved lockfile"));

    let ratio = compressed.len() as f32 / output.len() as f32;
    assert!(ratio < 0.15, "ratio was {ratio}");
}

#[test]
fn pnpm_install_limits_progress_and_keeps_auth_warning_error_summary() {
    let mut output =
        String::from("Lockfile is up to date\nAlready up-to-date\nAlready up-to-date\n");
    for index in 0..12 {
        output.push_str(&format!(
            "Progress: resolved {}, reused {}, downloaded {}, added {}\n",
            index * 10,
            index,
            index + 1,
            index + 2
        ));
    }
    output.push_str("WARN GET_NO_AUTH 401 https://registry.example/private\nERR_PNPM_FETCH_401 No authorization header was set\ndependencies:\n+ react 18.2.0\n- left-pad 1.3.0\nDone in 4.2s\n");

    let compressed = PnpmCompressor.compress("pnpm install", &output);
    assert_eq!(compressed.matches("Progress: resolved").count(), 2);
    assert_eq!(compressed.matches("Already up-to-date").count(), 1);
    assert!(compressed.contains("WARN GET_NO_AUTH"));
    assert!(compressed.contains("ERR_PNPM_FETCH_401"));
    assert!(compressed.contains("dependencies:"));
    assert!(compressed.contains("Done in 4.2s"));

    let ratio = compressed.len() as f32 / output.len() as f32;
    assert!(ratio < 0.45, "ratio was {ratio}");
}

#[test]
fn pytest_drops_passes_keeps_failures_summary_and_warning_cap() {
    let mut output = String::from("============================= test session starts =============================\nplatform darwin -- Python 3.12.1, pytest-8.1.1\nrootdir: /repo\ncollected 45 items\n\ntests/test_ok.py ............................ PASSED\ntests/test_more.py sssxxx PASSED\ntests/test_bad.py::test_breaks FAILED\n\n=================================== FAILURES ===================================\n______________________________ test_breaks ______________________________\nE   AssertionError: boom\n\n=============================== warnings summary ===============================\n");
    for index in 0..8 {
        output.push_str(&format!(
            "tests/test_warn.py:{index}: DeprecationWarning: deprecated api {index}\n"
        ));
    }
    output.push_str("=========================== short test summary info ===========================\nFAILED tests/test_bad.py::test_breaks - AssertionError: boom\n==================== 44 passed, 1 failed, 3 skipped in 2.34s ====================\n");

    let compressed = PytestCompressor.compress("python -m pytest", &output);
    assert!(compressed.contains("platform darwin"));
    assert!(compressed.contains("rootdir: /repo"));
    assert!(compressed.contains("collected 45 items"));
    assert!(!compressed.contains("tests/test_ok.py"));
    assert!(compressed.contains("tests/test_bad.py::test_breaks FAILED"));
    assert!(compressed.contains("AssertionError: boom"));
    assert!(compressed.contains("... and 3 more warnings"));
    assert!(compressed.contains("short test summary info"));
    assert!(compressed.contains("44 passed, 1 failed"));

    let ratio = compressed.len() as f32 / output.len() as f32;
    assert!(ratio < 0.80, "ratio was {ratio}");
}

#[test]
fn tsc_groups_errors_by_file_and_handles_clean_output() {
    let mut output = String::from(
        "Project 'tsconfig.json' is out of date because output is older than input\nCompiling...\n",
    );
    for index in 0..35 {
        output.push_str(&format!(
            "src/big.ts({},{}): error TS2322: Type 'string' is not assignable to type 'number'.\n",
            index + 1,
            index + 2
        ));
    }
    for file in 0..22 {
        output.push_str(&format!(
            "src/file_{file}.ts(1,1): error TS2304: Cannot find name 'missing{file}'.\n"
        ));
    }
    output.push_str("Found 57 errors in 23 files.\n");

    let compressed = TscCompressor.compress("pnpm tsc --noEmit", &output);
    assert!(!compressed.contains("Compiling..."));
    assert!(compressed.contains("src/big.ts(1,2): error TS2322"));
    assert!(compressed.contains("... and 25 more errors in this file"));
    assert!(compressed.contains("... and 13 more files with errors"));
    assert!(compressed.contains("Found 57 errors in 23 files"));

    let clean = TscCompressor.compress("tsc --noEmit", "Project build started\nCompiling...\n");
    assert_eq!(clean, "No errors. [cmpaft]");

    let ratio = compressed.len() as f32 / output.len() as f32;
    assert!(ratio < 0.45, "ratio was {ratio}");
}

#[test]
fn dispatch_reaches_extra_compressors() {
    let ctx = compress_context();
    let output = "Progress: resolved 1, reused 0, downloaded 0, added 0\nProgress: resolved 2, reused 0, downloaded 0, added 0\nProgress: resolved 3, reused 0, downloaded 0, added 0\ndependencies:\n+ zod 3.22.0\n".to_string();

    let compressed = compress::compress("pnpm install", output, &ctx);
    assert_eq!(compressed.matches("Progress: resolved").count(), 2);
    assert!(compressed.contains("dependencies:"));
}
