#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

fn init_repo() -> TempDir {
    let directory = tempfile::tempdir().expect("temp repo");
    run_git(directory.path(), &["init", "-q"]);
    run_git(
        directory.path(),
        &["config", "user.email", "test@example.com"],
    );
    run_git(directory.path(), &["config", "user.name", "Test"]);
    run_git(directory.path(), &["config", "commit.gpgSign", "false"]);
    fs::write(directory.path().join("README.md"), "initial\n").unwrap();
    run_git(directory.path(), &["add", "."]);
    run_git(directory.path(), &["commit", "-qm", "initial"]);
    directory
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("git command");
    assert!(status.success(), "git command failed: {args:?}");
}

fn repository_id(root: &Path) -> String {
    let common = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .expect("git common dir");
    assert!(common.status.success());
    let common = PathBuf::from(String::from_utf8(common.stdout).unwrap().trim());
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };
    let common = fs::canonicalize(common).unwrap();
    let identity = if common.file_name().and_then(|name| name.to_str()) == Some(".git") {
        common.parent().unwrap_or(&common)
    } else {
        &common
    };
    let name = identity
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("{name}-{}", stable_hash(&common.to_string_lossy()))
}

fn stable_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[test]
fn run_execs_the_agent_directly_in_the_managed_worktree_by_default() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let config_dir = home.path().join(".config/david");
    fs::create_dir_all(&config_dir).unwrap();

    let agent_log = home.path().join("agent.log");
    let agent = home.path().join("agent.sh");
    fs::write(
        &agent,
        "#!/bin/sh\n{ pwd; printf '%s\\n' \"$@\"; } > \"$DAVID_AGENT_LOG\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&agent).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&agent, permissions).unwrap();

    fs::write(
        config_dir.join("config.toml"),
        format!(
            "default_agent = \"test\"\n\n[agents.test]\ncommand = \"{}\"\nargs = []\n",
            agent.display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_david"))
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("XDG_STATE_HOME", home.path().join(".local/state"))
        .env("DAVID_AGENT_LOG", &agent_log)
        .args([
            "run",
            "--no-interactive",
            "feature-login",
            "--",
            "--model",
            "gpt 5.6",
        ])
        .output()
        .expect("david run");

    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let worktree = home
        .path()
        .join(".local/share/david/worktrees")
        .join(repository_id(repo.path()))
        .join("feature-login");
    let worktree = fs::canonicalize(worktree).unwrap();
    assert_eq!(
        fs::read_to_string(agent_log).unwrap(),
        format!("{}\n--model\ngpt 5.6\n", worktree.display())
    );

    let session_dir = home.path().join(".local/state/david/sessions");
    assert!(
        !session_dir.exists() || fs::read_dir(session_dir).unwrap().next().is_none(),
        "direct execution must not leave managed session state"
    );
}
