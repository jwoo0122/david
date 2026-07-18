use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::Duration,
};
use tempfile::TempDir;

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("git command");
    assert!(status.success(), "git command failed: {args:?}");
}

fn init_repo() -> TempDir {
    let directory = tempfile::tempdir().expect("temp repo");
    run_git(directory.path(), &["init", "-q"]);
    run_git(
        directory.path(),
        &["config", "user.email", "test@example.com"],
    );
    run_git(directory.path(), &["config", "user.name", "Test"]);
    fs::write(directory.path().join("README.md"), "initial\n").unwrap();
    run_git(directory.path(), &["add", "."]);
    run_git(directory.path(), &["commit", "-qm", "initial"]);
    directory
}

fn tmux_available() -> bool {
    let Ok(output) = Command::new("tmux")
        .args(["-f", "/dev/null", "-V"])
        .output()
    else {
        return false;
    };
    output.status.success() && supports_extended_keys(&String::from_utf8_lossy(&output.stdout))
}

fn supports_extended_keys(version: &str) -> bool {
    let mut numbers = version
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|component| component.parse::<u32>().ok());
    let Some(major) = numbers.next() else {
        return false;
    };
    let Some(minor) = numbers.next() else {
        return false;
    };
    major > 3 || major == 3 && minor >= 2
}

fn tmux_with_tmpdir(tmpdir: &Path) -> Command {
    let mut command = Command::new("tmux");
    command
        .args(["-f", "/dev/null"])
        .env_remove("TMUX")
        .env("TMUX_TMPDIR", tmpdir);
    command
}

struct TmuxTestServer {
    tmpdir: PathBuf,
    sentinel: &'static str,
}

impl TmuxTestServer {
    fn new(home: &Path) -> Self {
        let tmpdir = home.join("tmux");
        fs::create_dir_all(&tmpdir).unwrap();
        let server = Self {
            tmpdir,
            sentinel: "david-test-sentinel",
        };
        assert!(
            tmux_with_tmpdir(&server.tmpdir)
                .args(["new-session", "-d", "-s", server.sentinel, "sleep", "300"])
                .status()
                .unwrap()
                .success()
        );
        server
    }

    fn configure(&self, command: &mut Command) {
        command.env("TMUX_TMPDIR", &self.tmpdir).env_remove("TMUX");
    }
}

impl Drop for TmuxTestServer {
    fn drop(&mut self) {
        let _ = tmux_with_tmpdir(&self.tmpdir)
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn run_david_with_agent(
    server: &TmuxTestServer,
    home: &Path,
    repo: &Path,
    args: &[&str],
    agent: Option<&str>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_david"));
    command
        .current_dir(repo)
        .env("HOME", home)
        .args(args)
        .stdin(Stdio::null());
    server.configure(&mut command);
    if let Some(agent) = agent {
        command.env("DAVID_AGENT", agent);
    } else {
        command.env_remove("DAVID_AGENT");
    }
    command.output().unwrap()
}

fn run_david(server: &TmuxTestServer, home: &Path, repo: &Path, args: &[&str]) -> Output {
    run_david_with_agent(server, home, repo, args, None)
}

fn write_config(home: &Path, content: &str) {
    let directory = home.join(".david");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("config.toml"), content).unwrap();
}

fn managed_feature(home: &Path) -> PathBuf {
    let repositories = fs::read_dir(home.join(".david/worktrees"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(repositories.len(), 1);
    repositories[0].join("feature")
}

#[test]
fn noninteractive_missing_selection_exits_two_without_waiting_or_creating() {
    if !tmux_available() {
        return;
    }

    for args in [
        vec!["run", "feature"],
        vec!["run", "--no-interactive", "feature"],
    ] {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        write_config(
            home.path(),
            "[agents.codex]\ncommand = \"echo\"\n\n[agents.claude]\ncommand = \"echo\"\n",
        );
        let server = TmuxTestServer::new(home.path());

        let output = run_david(&server, home.path(), repo.path(), &args);

        assert_eq!(output.status.code(), Some(2), "stderr: {:?}", output.stderr);
        assert!(String::from_utf8_lossy(&output.stderr).contains("non-interactive"));
        assert!(!home.path().join(".david/worktrees").exists());
    }
}

#[test]
fn environment_agent_overrides_another_configured_default() {
    if !tmux_available() {
        return;
    }

    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    write_config(
        home.path(),
        "default_agent = \"other\"\n\n[agents.codex]\ncommand = \"sleep\"\nargs = [\"30\"]\n\n[agents.other]\ncommand = \"missing-agent\"\n",
    );
    let server = TmuxTestServer::new(home.path());

    let output = run_david_with_agent(
        &server,
        home.path(),
        repo.path(),
        &["run", "--no-interactive", "feature"],
        Some("codex"),
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert!(managed_feature(home.path()).is_dir());
    let cleanup = run_david(
        &server,
        home.path(),
        repo.path(),
        &["remove", "--force", "feature"],
    );
    assert_eq!(
        cleanup.status.code(),
        Some(0),
        "stderr: {:?}",
        cleanup.stderr
    );
}

#[cfg(unix)]
#[test]
fn noninteractive_run_uses_default_agent_and_literal_runtime_argv_without_attach() {
    if !tmux_available() {
        return;
    }

    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let output_file = home.path().join("argv");
    let script = home.path().join("agent.sh");
    let script_content = format!(
        "#!/bin/sh\n: > '{}'\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{}'; done\nsleep 30\n",
        output_file.display(),
        output_file.display()
    );
    fs::write(&script, script_content).unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();
    write_config(
        home.path(),
        &format!(
            "default_agent = \"codex\"\n\n[agents.codex]\ncommand = {:?}\nargs = [\"configured\"]\n\n[agents.other]\ncommand = \"missing-agent\"\n",
            script.to_string_lossy()
        ),
    );
    let server = TmuxTestServer::new(home.path());

    let output = run_david(
        &server,
        home.path(),
        repo.path(),
        &[
            "run",
            "--no-interactive",
            "feature",
            "--",
            "--model",
            "gpt 5.6",
            "$()",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    let target = managed_feature(home.path());
    assert!(target.is_dir());
    for _ in 0..100 {
        if output_file.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(&output_file).unwrap(),
        "configured\n--model\ngpt 5.6\n$()\n"
    );

    let cleanup = run_david(
        &server,
        home.path(),
        repo.path(),
        &["remove", "--force", "feature"],
    );
    assert_eq!(
        cleanup.status.code(),
        Some(0),
        "stderr: {:?}",
        cleanup.stderr
    );
}
