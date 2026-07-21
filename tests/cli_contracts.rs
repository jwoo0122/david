#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
#[cfg(target_os = "linux")]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};
use tempfile::TempDir;

fn david(home: &Path, cwd: &Path, args: &[&str], fake_tmux: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_david"));
    command.current_dir(cwd).env("HOME", home).args(args);
    if let Some(fake_tmux) = fake_tmux {
        let original = env::var_os("PATH").unwrap_or_default();
        let path = env::join_paths(
            std::iter::once(fake_tmux.to_path_buf()).chain(env::split_paths(&original)),
        )
        .unwrap();
        command.env("PATH", path);
    }
    command.output().expect("david command")
}

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

fn add_worktree(root: &Path, target: &Path, branch: &str) {
    let status = Command::new("git")
        .current_dir(root)
        .args(["worktree", "add", "-q", "-b", branch])
        .arg(target)
        .arg("HEAD")
        .status()
        .expect("git worktree add");
    assert!(status.success(), "git worktree add failed");
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

fn fake_tmux() -> TempDir {
    let directory = tempfile::tempdir().expect("fake tmux directory");
    let program = directory.path().join("tmux");
    fs::write(
        &program,
        "#!/bin/sh\ncase \" $* \" in\n  *\" -V \"*) printf '%s\\n' 'tmux 3.4' ;;\nesac\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(program, permissions).unwrap();
    directory
}

fn fake_tmux_without_sessions() -> TempDir {
    let directory = tempfile::tempdir().expect("fake tmux directory");
    let program = directory.path().join("tmux");
    fs::write(
        &program,
        "#!/bin/sh\ncase \" $* \" in\n  *\" -V \"*) printf '%s\\n' 'tmux 3.4'; exit 0 ;;\n  *\" has-session \"*) exit 1 ;;\nesac\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(program, permissions).unwrap();
    directory
}

#[test]
fn list_human_bytes_and_empty_porcelain_output_are_stable() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let tmux = fake_tmux();

    let human = david(home.path(), repo.path(), &["list"], Some(tmux.path()));
    assert_eq!(human.status.code(), Some(0));
    assert!(human.stderr.is_empty());
    assert_eq!(
        human.stdout,
        b"No managed worktrees.\n"
    );

    let porcelain = david(
        home.path(),
        repo.path(),
        &["list", "--porcelain"],
        Some(tmux.path()),
    );
    assert_eq!(porcelain.status.code(), Some(0));
    assert!(porcelain.stdout.is_empty());
    assert!(porcelain.stderr.is_empty());

    let nul = david(
        home.path(),
        repo.path(),
        &["list", "--porcelain", "-z"],
        Some(tmux.path()),
    );
    assert_eq!(nul.status.code(), Some(0));
    assert!(nul.stdout.is_empty());
    assert!(nul.stderr.is_empty());
}

#[test]
fn path_outputs_lf_or_nul_and_rejects_missing_worktrees() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let target = home
        .path()
        .join(".david")
        .join("worktrees")
        .join(repository_id(repo.path()))
        .join("feature");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    add_worktree(repo.path(), &target, "feature");
    let expected = fs::canonicalize(&target).unwrap();

    let lf = david(home.path(), repo.path(), &["path", "feature"], None);
    assert_eq!(lf.status.code(), Some(0));
    assert!(lf.stderr.is_empty());
    let mut expected_lf = expected.as_os_str().as_encoded_bytes().to_vec();
    expected_lf.push(b'\n');
    assert_eq!(lf.stdout, expected_lf);

    let nul = david(home.path(), repo.path(), &["path", "-0", "feature"], None);
    assert_eq!(nul.status.code(), Some(0));
    assert!(nul.stderr.is_empty());
    let mut expected_nul = expected.as_os_str().as_encoded_bytes().to_vec();
    expected_nul.push(b'\0');
    assert_eq!(nul.stdout, expected_nul);

    let missing = david(home.path(), repo.path(), &["path", "missing"], None);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert_eq!(
        missing.stderr,
        b"error: managed worktree does not exist: missing\n"
    );
}

#[test]
fn porcelain_nul_output_preserves_a_newline_in_a_worktree_path() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let tmux = fake_tmux();
    let target = home
        .path()
        .join(".david")
        .join("worktrees")
        .join(repository_id(repo.path()))
        .join("feature\nwith-newline");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    add_worktree(repo.path(), &target, "feature");
    let target = fs::canonicalize(target).unwrap();

    let lf = david(
        home.path(),
        repo.path(),
        &["list", "--porcelain"],
        Some(tmux.path()),
    );
    assert_eq!(lf.status.code(), Some(0));
    assert!(lf.stderr.is_empty());
    let mut expected_lf =
        b"name feature\nwith-newline\nbranch feature\nagent -\nsession unknown\npath ".to_vec();
    expected_lf.extend_from_slice(target.as_os_str().as_encoded_bytes());
    expected_lf.push(b'\n');
    assert_eq!(lf.stdout, expected_lf);

    let nul = david(
        home.path(),
        repo.path(),
        &["list", "--porcelain", "-z"],
        Some(tmux.path()),
    );
    assert_eq!(nul.status.code(), Some(0));
    assert!(nul.stderr.is_empty());
    let mut expected_nul =
        b"name feature\nwith-newline\0branch feature\0agent -\0session unknown\0path ".to_vec();
    expected_nul.extend_from_slice(target.as_os_str().as_encoded_bytes());
    expected_nul.push(b'\0');
    assert_eq!(nul.stdout, expected_nul);
}

#[test]
fn invalid_agent_argument_exits_with_a_controlled_configuration_error() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let config_dir = home.path().join(".david");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "[agents.test]\ncommand = \"echo\"\nargs = [\"bad\\u0000arg\"]\n",
    )
    .unwrap();
    let tmux = fake_tmux_without_sessions();

    let output = david(
        home.path(),
        repo.path(),
        &["run", "feature"],
        Some(tmux.path()),
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"error: agent \"test\" arguments must not contain NUL bytes\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn actual_git_worktree_invalid_path_bytes_are_preserved_by_list_and_path() {
    let repo = init_repo();
    let home_root = tempfile::tempdir().unwrap();
    let home = home_root
        .path()
        .join(OsString::from_vec(b"home-\xff".to_vec()));
    fs::create_dir(&home).unwrap();
    let target = home
        .join(".david")
        .join("worktrees")
        .join(repository_id(repo.path()))
        .join("feature");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    add_worktree(repo.path(), &target, "feature");
    let expected = fs::canonicalize(&target).unwrap();

    let path = david(
        home.as_path(),
        repo.path(),
        &["path", "-0", "feature"],
        None,
    );
    assert_eq!(path.status.code(), Some(0));
    assert!(path.stderr.is_empty());
    let mut expected_path = expected.as_os_str().as_encoded_bytes().to_vec();
    expected_path.push(b'\0');
    assert_eq!(path.stdout, expected_path);

    let tmux = fake_tmux();
    let list = david(
        home.as_path(),
        repo.path(),
        &["list", "--porcelain", "-z"],
        Some(tmux.path()),
    );
    assert_eq!(list.status.code(), Some(0));
    assert!(list.stderr.is_empty());
    let mut expected_list =
        b"name feature\0branch feature\0agent -\0session unknown\0path ".to_vec();
    expected_list.extend_from_slice(expected.as_os_str().as_encoded_bytes());
    expected_list.push(b'\0');
    assert_eq!(list.stdout, expected_list);
}

#[test]
fn cli_rejects_list_zero_without_porcelain_and_documents_new_options() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();

    let invalid = david(home.path(), repo.path(), &["list", "-z"], None);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());

    let list_help = david(home.path(), repo.path(), &["list", "--help"], None);
    assert_eq!(list_help.status.code(), Some(0));
    assert!(list_help.stderr.is_empty());
    let list_help = String::from_utf8_lossy(&list_help.stdout);
    assert!(list_help.contains("--porcelain"));
    assert!(list_help.contains("-z"));

    let path_help = david(home.path(), repo.path(), &["path", "--help"], None);
    assert_eq!(path_help.status.code(), Some(0));
    assert!(path_help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&path_help.stdout).contains("-0"));
}

#[test]
fn run_without_name_in_noninteractive_mode_exits_one() {
    let repo = init_repo();
    let home = tempfile::tempdir().unwrap();
    let tmux = fake_tmux();

    let output = david(
        home.path(),
        repo.path(),
        &["run", "--no-interactive"],
        Some(tmux.path()),
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("non-interactive"));
    assert!(!home.path().join(".david/worktrees").exists());
}
