use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Output, Stdio};

pub struct TestEnvironment {
    runtime: tempfile::TempDir,
}

impl TestEnvironment {
    pub fn new() -> Self {
        Self {
            runtime: tempfile::tempdir().expect("create isolated test runtime"),
        }
    }

    pub fn run<I, S>(&self, arguments: I, stdin: Option<&str>) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.command();
        command.args(arguments);
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command.spawn().expect("start lighter");
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .expect("lighter stdin is piped")
                .write_all(input.as_bytes())
                .expect("write lighter stdin");
        }
        child.wait_with_output().expect("wait for lighter")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lighter"));
        command
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("CARGO_TARGET_DIR", self.runtime.path().join("cargo-target"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        let _ = self
            .command()
            .args(["daemon", "kill"])
            .stdin(Stdio::null())
            .output();
    }
}
