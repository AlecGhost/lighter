mod support;

use std::fs;
use std::process::Output;

use support::TestEnvironment;

fn snapshot(output: &Output) -> String {
    let status = match output.status.code() {
        Some(code) => code.to_string(),
        None => "terminated by signal".to_owned(),
    };
    format!(
        "exit: {status}\n--- stdout ---\n{}<EOF>\n--- stderr ---\n{}<EOF>",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

#[test]
fn help_is_the_public_binary_interface() {
    let environment = TestEnvironment::new();
    let output = environment.run(["--help"], None);

    assert!(output.status.success());
    insta::assert_snapshot!("help", snapshot(&output));
}

#[test]
fn clap_rejects_invalid_invocations_before_io() {
    let environment = TestEnvironment::new();

    let conflicting = environment.run(
        [
            "daemon",
            "spawn",
            "--format",
            "html",
            "tests/demo-projects/rust/src/main.rs",
        ],
        None,
    );
    let invalid_range = environment.run(
        [
            "--lang",
            "rust",
            "--lines",
            "3:2",
            "--no-lsp",
            "--no-tree-sitter",
        ],
        Some("fn main() {}\n"),
    );

    insta::assert_snapshot!("clap_conflicting_invocation", snapshot(&conflicting));
    insta::assert_snapshot!("clap_invalid_line_range", snapshot(&invalid_range));
}

#[test]
fn stdin_requires_a_language() {
    let environment = TestEnvironment::new();
    let output = environment.run(["--no-lsp", "--no-tree-sitter"], Some("plain input"));

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    insta::assert_snapshot!("stdin_without_language", snapshot(&output));
}

#[test]
fn disabled_engines_still_select_and_escape_input() {
    let environment = TestEnvironment::new();
    let source = "first <tag>\nsecond & value\nthird \\\\ {value}\n";

    for (format, snapshot_name) in [
        ("ansi", "plain_ansi"),
        ("html", "plain_html"),
        ("latex", "plain_latex"),
    ] {
        let output = environment.run(
            [
                "--lang",
                "rust",
                "--format",
                format,
                "--lines",
                "2:3",
                "--no-lsp",
                "--no-tree-sitter",
            ],
            Some(source),
        );

        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(output.stderr.is_empty());
        insta::assert_snapshot!(snapshot_name, snapshot(&output));
    }
}

#[test]
fn file_name_detection_and_explicit_language_both_work() {
    let environment = TestEnvironment::new();
    let source = "tests/demo-projects/rust/src/main.rs";

    let detected = environment.run(
        [source, "--format", "html", "--lines", "1:2", "--no-lsp"],
        None,
    );
    let explicit = environment.run(
        [
            source, "--lang", "rust", "--format", "html", "--lines", "1:2", "--no-lsp",
        ],
        None,
    );

    assert!(detected.status.success(), "{:?}", detected.stderr);
    assert_eq!(detected.stdout, explicit.stdout);
    insta::assert_snapshot!("file_language_detection", snapshot(&detected));
}

#[test]
fn operation_errors_use_stderr_without_a_successful_fragment() {
    let environment = TestEnvironment::new();
    let missing_source = environment.run(["tests/demo-projects/rust/src/missing.rs"], None);

    let config = environment.run(
        [
            "--lang",
            "rust",
            "--config",
            "tests/demo-projects/missing-config.toml",
            "--no-lsp",
        ],
        Some("fn main() {}\n"),
    );

    for output in [&missing_source, &config] {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .filter(|character| *character == '\n')
                .count(),
            1
        );
    }

    insta::assert_snapshot!("missing_source", snapshot(&missing_source));
    insta::assert_snapshot!("missing_config", snapshot(&config));
}

#[test]
fn configured_server_failures_are_reported_as_highlight_errors() {
    let environment = TestEnvironment::new();
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("lighter.toml");
    fs::write(
        &config,
        "[servers]\nrust = \"lighter-language-server-does-not-exist\"\n",
    )
    .unwrap();

    let output = environment.run(
        [
            "--lang",
            "rust",
            "--config",
            config.to_str().unwrap(),
            "--no-tree-sitter",
        ],
        Some("fn main() {}\n"),
    );

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    insta::assert_snapshot!("missing_language_server", snapshot(&output));
}

#[test]
fn daemon_lifecycle_and_request_overrides_are_observable() {
    let environment = TestEnvironment::new();
    let spawn = environment.run(["daemon", "spawn", "--format", "html"], None);
    assert!(spawn.status.success(), "{:?}", spawn.stderr);

    let default_format = environment.run(
        ["--lang", "rust", "--no-lsp", "--no-tree-sitter"],
        Some("<daemon default>\n"),
    );
    let request_override = environment.run(
        [
            "--lang",
            "rust",
            "--format",
            "latex",
            "--no-lsp",
            "--no-tree-sitter",
        ],
        Some("\\override{works}\n"),
    );
    let kill = environment.run(["daemon", "kill"], None);
    let second_kill = environment.run(["daemon", "kill"], None);

    assert!(
        default_format.status.success(),
        "{:?}",
        default_format.stderr
    );
    assert!(
        request_override.status.success(),
        "{:?}",
        request_override.stderr
    );
    assert!(kill.status.success(), "{:?}", kill.stderr);
    assert!(!second_kill.status.success());

    insta::assert_snapshot!(
        "daemon_lifecycle",
        format!(
            "spawn:\n{}\ndefault request:\n{}\noverridden request:\n{}\nkill:\n{}\nsecond kill:\n{}",
            snapshot(&spawn),
            snapshot(&default_format),
            snapshot(&request_override),
            snapshot(&kill),
            snapshot(&second_kill),
        )
    );
}
