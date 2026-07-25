mod support;

use std::path::{Path, PathBuf};

use support::TestEnvironment;

fn assert_semantic_snapshot(language: &str, server: &str, source: &Path, snapshot_name: &str) {
    if !require_or_skip(server) {
        return;
    }

    let environment = TestEnvironment::new();
    let project = source
        .ancestors()
        .find(|path| path.join(project_marker(language)).exists())
        .expect("source belongs to a demo project");
    let output = environment.run(
        [
            "--lang",
            language,
            "--project",
            project.to_str().unwrap(),
            "--format",
            "html",
            "--no-tree-sitter",
            source.to_str().unwrap(),
        ],
        None,
    );

    assert!(
        output.status.success(),
        "{language} language server failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{language} unexpectedly wrote to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8(output.stdout).expect("lighter output is UTF-8");
    assert!(
        rendered.contains("<a-"),
        "{language} output has no semantic highlight elements"
    );
    insta::assert_snapshot!(snapshot_name, rendered);
}

fn project_marker(language: &str) -> &'static str {
    match language {
        "rust" => "Cargo.toml",
        "python" => "pyproject.toml",
        "typescript" => "tsconfig.json",
        "go" => "go.mod",
        _ => unreachable!("unsupported semantic fixture language"),
    }
}

fn project(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("demo-projects")
        .join(name)
}

fn executable_on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .any(|directory| executable_in_directory(&directory, program))
}

#[cfg(unix)]
fn executable_in_directory(directory: &Path, program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    directory
        .join(program)
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn executable_in_directory(directory: &Path, program: &str) -> bool {
    std::env::var_os("PATHEXT")
        .and_then(|extensions| extensions.to_str().map(str::to_owned))
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned())
        .split(';')
        .any(|extension| directory.join(format!("{program}{extension}")).is_file())
}

fn require_or_skip(program: &str) -> bool {
    require_or_skip_condition(
        executable_on_path(program),
        format!("language server '{program}' is not available on PATH"),
    )
}

fn require_project_file_or_skip(path: &Path) -> bool {
    require_or_skip_condition(
        path.exists(),
        format!(
            "required demo-project dependency '{}' is missing",
            path.display()
        ),
    )
}

fn require_or_skip_condition(available: bool, message: String) -> bool {
    if available {
        return true;
    }

    if std::env::var_os("LIGHTER_REQUIRE_LANGUAGE_SERVERS").is_some() {
        panic!("{message}");
    }
    eprintln!("skipping semantic integration test: {message}");
    false
}

#[test]
fn rust_uses_rust_analyzer_semantic_tokens() {
    assert_semantic_snapshot(
        "rust",
        "rust-analyzer",
        &project("rust").join("src/main.rs"),
        "semantic_rust",
    );
}

#[test]
fn python_uses_basedpyright_semantic_tokens() {
    assert_semantic_snapshot(
        "python",
        "basedpyright-langserver",
        &project("python").join("app.py"),
        "semantic_python",
    );
}

#[test]
fn typescript_uses_typescript_language_server_semantic_tokens() {
    let project = project("typescript");
    if !require_project_file_or_skip(
        &project.join("node_modules/typescript/lib/tsserverlibrary.js"),
    ) {
        return;
    }
    assert_semantic_snapshot(
        "typescript",
        "typescript-language-server",
        &project.join("src/index.ts"),
        "semantic_typescript",
    );
}

#[test]
fn go_uses_gopls_semantic_tokens() {
    assert_semantic_snapshot("go", "gopls", &project("go").join("main.go"), "semantic_go");
}
