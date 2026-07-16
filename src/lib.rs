use dialoguer::{Input, Select, theme::ColorfulTheme};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{DirBuilderExt, OpenOptionsExt},
};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, ToolError>;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("configuration parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("configuration serialization error: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),
    #[error("{program} failed: {detail}")]
    Command { program: String, detail: String },
}

#[derive(Clone, Debug)]
pub struct DavidPaths {
    worktrees: PathBuf,
    sessions: PathBuf,
    config: PathBuf,
}

impl DavidPaths {
    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let root = home.join(".david");
        Self {
            worktrees: root.join("worktrees"),
            sessions: root.join("sessions"),
            config: root.join("config.toml"),
        }
    }

    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| ToolError::Message("HOME is not set".to_owned()))?;
        Ok(Self::from_home(home))
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }

    pub fn setup(&self) -> Result<()> {
        self.setup_with(TerminalSetupPrompter)
    }

    fn setup_with<P: SetupPrompter>(&self, prompter: P) -> Result<()> {
        let config = Config::load_or_default(&self.config)?;
        let config = prompter.collect(config)?;
        config.validate()?;
        self.prepare()?;
        write_config(&self.config, &config)?;
        println!("Agent configuration written to {}", self.config.display());
        Ok(())
    }

    fn prepare(&self) -> Result<()> {
        fs::create_dir_all(&self.worktrees)?;
        fs::create_dir_all(&self.sessions)?;
        Ok(())
    }

    fn repository_worktrees(&self, repo_id: &str) -> PathBuf {
        self.worktrees.join(repo_id)
    }

    fn worktree_path(&self, repo_id: &str, name: &str) -> PathBuf {
        self.repository_worktrees(repo_id).join(name)
    }

    fn session_state_path(&self, repo_id: &str, name: &str) -> PathBuf {
        self.sessions
            .join(format!("{}-{}.state", repo_id, stable_hash(name)))
    }

    fn validate_worktree_path(&self, repo_id: &str, name: &str) -> Result<()> {
        let path_error = || {
            ToolError::Message(format!(
                "managed worktree path escapes the managed directory: {name}"
            ))
        };
        let canonical_worktrees = canonicalize_with_missing(&self.worktrees)?;
        let base = self.repository_worktrees(repo_id);
        let canonical_base = canonicalize_with_missing(&base).map_err(|_| path_error())?;
        let target = self.worktree_path(repo_id, name);
        let canonical_target = canonicalize_with_missing(&target).map_err(|_| path_error())?;

        if !canonical_base.starts_with(&canonical_worktrees)
            || !canonical_target.starts_with(&canonical_base)
        {
            return Err(path_error());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Agent {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub agents: BTreeMap<String, Agent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMetadata {
    pub project_name: String,
    pub worktree_name: String,
    pub agent_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionState {
    repo_id: String,
    worktree_name: String,
    worktree_path: PathBuf,
    branch: String,
    session: String,
    agent: String,
    pane: Option<String>,
}

impl SessionState {
    fn matches(&self, repo_id: &str, name: &str, target: &Path, session: &str) -> bool {
        self.repo_id == repo_id
            && self.worktree_name == name
            && self.branch == name
            && self.session == session
            && same_path(&self.worktree_path, target)
    }

    fn encode(&self) -> String {
        let mut encoded = format!(
            "repo_id={}\nworktree_name={}\nworktree_path_hex={}\nbranch={}\nsession={}\nagent={}\n",
            self.repo_id,
            self.worktree_name,
            encode_path_hex(&self.worktree_path),
            self.branch,
            self.session,
            self.agent
        );
        if let Some(pane) = &self.pane {
            encoded.push_str("pane=");
            encoded.push_str(pane);
            encoded.push('\n');
        }
        encoded
    }
}

impl Config {
    fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::load(path)
    }

    fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ToolError::Message(format!(
                    "agent configuration not found at {}; add an [agents.<name>] entry",
                    path.display()
                ))
            } else {
                ToolError::Io(error)
            }
        })?;
        let config: Self = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.agents.is_empty() {
            return Err(ToolError::Message(
                "agent configuration must contain at least one agent".to_owned(),
            ));
        }
        for (name, agent) in &self.agents {
            if name.trim().is_empty()
                || name.contains('\n')
                || name.contains('\r')
                || name.contains('\0')
            {
                return Err(ToolError::Message(
                    "agent names must be non-empty single-line values".to_owned(),
                ));
            }
            if agent.command.trim().is_empty()
                || agent.command.contains('\n')
                || agent.command.contains('\r')
                || agent.command.contains('\0')
            {
                return Err(ToolError::Message(format!(
                    "agent {name:?} must define a non-empty single-line command"
                )));
            }
            if agent.args.iter().any(|argument| argument.contains('\0')) {
                return Err(ToolError::Message(format!(
                    "agent {name:?} arguments must not contain NUL bytes"
                )));
            }
        }
        Ok(())
    }

    fn add_or_replace(&mut self, name: String, agent: Agent) -> Result<()> {
        let mut candidate = self.clone();
        candidate.agents.insert(name, agent);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}

trait SetupPrompter {
    fn collect(&self, config: Config) -> Result<Config>;
}

#[derive(Clone, Copy, Debug, Default)]
struct TerminalSetupPrompter;

impl SetupPrompter for TerminalSetupPrompter {
    fn collect(&self, mut config: Config) -> Result<Config> {
        print_agents(&config);
        loop {
            let name = prompt_text("Agent name (Enter to finish)", true)?;
            let name = name.trim().to_owned();
            if name.is_empty() {
                break;
            }

            let command = prompt_text("Command", false)?.trim().to_owned();
            let args = loop {
                let raw = prompt_text("Arguments (optional)", true)?;
                match parse_agent_arguments(&raw) {
                    Ok(args) => break args,
                    Err(error) => eprintln!("Invalid arguments: {error}"),
                }
            };

            config.add_or_replace(name, Agent { command, args })?;
            print_agents(&config);
        }
        Ok(config)
    }
}

fn prompt_text(prompt: &str, allow_empty: bool) -> Result<String> {
    let theme = ColorfulTheme::default();
    let input = Input::<String>::with_theme(&theme).with_prompt(prompt);
    let input = if allow_empty {
        input.allow_empty(true)
    } else {
        input
    };
    input
        .interact_text()
        .map_err(io::Error::from)
        .map_err(Into::into)
}

fn parse_agent_arguments(raw: &str) -> Result<Vec<String>> {
    shell_words::split(raw)
        .map_err(|error| ToolError::Message(format!("could not parse agent arguments: {error}")))
}

fn print_agents(config: &Config) {
    println!("\nConfigured agents:");
    if config.agents.is_empty() {
        println!("  (none)");
        return;
    }
    for (name, agent) in &config.agents {
        println!("  {name}: {} {:?}", agent.command, agent.args);
    }
}

pub trait AgentPicker {
    fn pick(&self, config: &Config) -> Result<(String, Agent)>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalAgentPicker;

impl AgentPicker for TerminalAgentPicker {
    fn pick(&self, config: &Config) -> Result<(String, Agent)> {
        let choices: Vec<(&String, &Agent)> = config.agents.iter().collect();
        let labels: Vec<String> = choices
            .iter()
            .map(|(name, agent)| format!("{name} ({})", agent.command))
            .collect();
        let index = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select an agent")
            .items(&labels)
            .default(0)
            .interact()
            .map_err(io::Error::from)?;
        let (name, agent) = choices[index];
        Ok((name.clone(), agent.clone()))
    }
}

#[derive(Clone, Debug)]
pub struct Git {
    program: OsString,
}

impl Default for Git {
    fn default() -> Self {
        Self::new("git")
    }
}

impl Git {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(&self.program);
        command.current_dir(cwd);
        command
    }

    fn output(&self, mut command: Command) -> Result<Output> {
        command.output().map_err(ToolError::Io).and_then(|output| {
            if output.status.success() {
                Ok(output)
            } else {
                Err(command_error("git", &output))
            }
        })
    }

    pub fn repository_root(&self, cwd: &Path) -> Result<PathBuf> {
        let mut command = self.command(cwd);
        command.args(["rev-parse", "--show-toplevel"]);
        let output = self.output(command)?;
        let path = PathBuf::from(text(&output.stdout).trim());
        fs::canonicalize(path).map_err(ToolError::Io)
    }

    fn common_git_dir(&self, root: &Path) -> Result<PathBuf> {
        let mut command = self.command(root);
        command.args(["rev-parse", "--git-common-dir"]);
        let output = self.output(command)?;
        let raw = PathBuf::from(text(&output.stdout).trim());
        let common_dir = if raw.is_absolute() {
            raw
        } else {
            root.join(raw)
        };
        fs::canonicalize(common_dir).map_err(ToolError::Io)
    }

    fn repository_id(&self, root: &Path) -> Result<String> {
        repository_id(&self.common_git_dir(root)?)
    }

    fn repository_name(&self, root: &Path) -> Result<String> {
        let common_dir = self.common_git_dir(root)?;
        let project_dir = if common_dir.file_name().and_then(|name| name.to_str()) == Some(".git") {
            common_dir.parent().unwrap_or(&common_dir).to_path_buf()
        } else {
            self.worktrees(root)?
                .into_iter()
                .next()
                .map(|worktree| worktree.path)
                .unwrap_or_else(|| root.to_path_buf())
        };
        project_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                ToolError::Message(format!(
                    "could not determine project name from source directory: {}",
                    project_dir.display()
                ))
            })
    }

    fn current_head(&self, root: &Path) -> Result<()> {
        let mut command = self.command(root);
        command.args(["rev-parse", "--verify", "HEAD"]);
        self.output(command).map(|_| ())
    }

    fn source_is_dirty(&self, root: &Path) -> Result<bool> {
        let mut command = self.command(root);
        command.args(["status", "--porcelain=v1", "--untracked-files=all"]);
        Ok(!self.output(command)?.stdout.is_empty())
    }

    fn validate_branch(&self, root: &Path, name: &str) -> Result<()> {
        let mut command = self.command(root);
        command.args(["check-ref-format", "--branch", name]);
        self.output(command).map(|_| ())
    }

    fn worktrees(&self, root: &Path) -> Result<Vec<Worktree>> {
        let mut command = self.command(root);
        command.args(["worktree", "list", "--porcelain", "-z"]);
        let output = self.output(command)?;
        Ok(parse_worktree_list_bytes(&output.stdout))
    }

    fn add_worktree(&self, root: &Path, name: &str, path: &Path) -> Result<()> {
        let mut command = self.command(root);
        command
            .args(["worktree", "add", "-b", name])
            .arg(path)
            .arg("HEAD");
        self.output(command).map(|_| ())
    }

    fn remove_worktree(&self, root: &Path, path: &Path, force: bool) -> Result<()> {
        let mut command = self.command(root);
        command.arg("worktree").arg("remove");
        if force {
            command.arg("--force");
        }
        command.arg(path);
        self.output(command).map(|_| ())
    }

    fn branch_command_root(&self, root: &Path, target: &Path) -> Result<PathBuf> {
        if !same_path(root, target) {
            return Ok(root.to_path_buf());
        }
        for worktree in self.worktrees(root)? {
            if !same_path(&worktree.path, target) && worktree.path.is_dir() {
                return Ok(worktree.path);
            }
        }
        Err(ToolError::Message(
            "cannot remove the current worktree without another worktree to operate from"
                .to_owned(),
        ))
    }

    fn delete_branch(&self, root: &Path, name: &str) -> Result<()> {
        let mut command = self.command(root);
        command.args(["branch", "-D", "--", name]);
        self.output(command).map(|_| ())
    }

    fn worktree_is_dirty(&self, worktree: &Path) -> Result<bool> {
        self.source_is_dirty(worktree)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStatus {
    Active,
    Inactive,
    Unknown,
}

impl SessionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManagedWorktree {
    name: String,
    branch: String,
    agent: String,
    session: SessionStatus,
    path: PathBuf,
}

pub fn parse_worktree_list_bytes(input: &[u8]) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;

    let mut finish = |current: &mut Option<Worktree>| {
        if let Some(worktree) = current.take() {
            worktrees.push(worktree);
        }
    };

    for field in input.split(|byte| *byte == 0) {
        if field.is_empty() {
            finish(&mut current);
            continue;
        }
        if let Some(path) = field.strip_prefix(b"worktree ") {
            finish(&mut current);
            current = Some(Worktree {
                path: path_from_bytes(path),
                head: String::new(),
                branch: None,
            });
        } else if let Some(head) = field.strip_prefix(b"HEAD ") {
            if let Some(worktree) = current.as_mut() {
                worktree.head = String::from_utf8_lossy(head).into_owned();
            }
        } else if let Some(branch) = field.strip_prefix(b"branch ")
            && let Some(worktree) = current.as_mut()
        {
            let branch = branch.strip_prefix(b"refs/heads/").unwrap_or(branch);
            worktree.branch = Some(String::from_utf8_lossy(branch).into_owned());
        }
    }
    finish(&mut current);
    worktrees
}

pub fn parse_worktree_list(input: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;

    let mut finish = |current: &mut Option<Worktree>| {
        if let Some(worktree) = current.take() {
            worktrees.push(worktree);
        }
    };

    for line in input.lines() {
        if line.is_empty() {
            finish(&mut current);
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            finish(&mut current);
            current = Some(Worktree {
                path: PathBuf::from(path),
                head: String::new(),
                branch: None,
            });
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            if let Some(worktree) = current.as_mut() {
                worktree.head = head.to_owned();
            }
        } else if let Some(branch) = line.strip_prefix("branch ")
            && let Some(worktree) = current.as_mut()
        {
            worktree.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_owned(),
            );
        }
    }
    finish(&mut current);
    worktrees
}

fn write_porcelain_list<W: Write>(
    entries: &[ManagedWorktree],
    zero: bool,
    output: &mut W,
) -> Result<()> {
    let delimiter = if zero { b'\0' } else { b'\n' };
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            output.write_all(&[delimiter])?;
        }
        write_porcelain_field(output, b"name ", &entry.name, delimiter)?;
        write_porcelain_field(output, b"branch ", &entry.branch, delimiter)?;
        write_porcelain_field(output, b"agent ", &entry.agent, delimiter)?;
        write_porcelain_field(output, b"session ", entry.session.as_str(), delimiter)?;
        output.write_all(b"path ")?;
        #[cfg(unix)]
        output.write_all(entry.path.as_os_str().as_bytes())?;
        #[cfg(not(unix))]
        output.write_all(entry.path.to_string_lossy().as_bytes())?;
        output.write_all(&[delimiter])?;
    }
    Ok(())
}

fn write_porcelain_field<W: Write>(
    output: &mut W,
    key: &[u8],
    value: &str,
    delimiter: u8,
) -> io::Result<()> {
    output.write_all(key)?;
    output.write_all(value.as_bytes())?;
    output.write_all(&[delimiter])
}

fn encode_path_hex(path: &Path) -> String {
    #[cfg(unix)]
    let bytes = path.as_os_str().as_bytes();
    #[cfg(not(unix))]
    let path = path.to_string_lossy();
    #[cfg(not(unix))]
    let bytes = path.as_bytes();

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_path_hex(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Some(path_from_bytes(&decoded))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

pub trait SessionBackend {
    fn ensure_available(&self) -> Result<()>;
    fn has_session(&self, name: &str) -> Result<bool>;
    fn create_session(&self, name: &str, cwd: &Path, agent: &Agent) -> Result<()>;
    fn create_session_with_pane(
        &self,
        name: &str,
        cwd: &Path,
        agent: &Agent,
    ) -> Result<Option<String>> {
        self.create_session(name, cwd, agent)?;
        self.agent_pane(name)
    }
    fn agent_pane(&self, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn pane_is_alive(&self, _name: &str, _pane: &str) -> Result<bool> {
        Ok(true)
    }
    fn configure_session(&self, _name: &str, _metadata: &SessionMetadata) -> Result<()> {
        Ok(())
    }
    fn attach(&self, name: &str) -> Result<()>;
    fn deliver_prompt(&self, _name: &str, _message: &str) -> Result<()> {
        Err(ToolError::Message(
            "session backend does not support prompt delivery".to_owned(),
        ))
    }
    fn deliver_prompt_to(&self, name: &str, message: &str, _pane: Option<&str>) -> Result<()> {
        self.deliver_prompt(name, message)
    }
    fn kill_session(&self, name: &str) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct TmuxBackend {
    program: OsString,
}

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::new("tmux")
    }
}

impl TmuxBackend {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(["-f", "/dev/null"]);
        command
    }

    fn status_error(&self, status: ExitStatus) -> ToolError {
        ToolError::Command {
            program: "tmux".to_owned(),
            detail: format!("exited with status {status}"),
        }
    }

    fn output(&self, mut command: Command) -> Result<Output> {
        let output = command.output()?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(command_error("tmux", &output))
        }
    }

    fn run_command(&self, command: Command) -> Result<()> {
        self.output(command).map(|_| ())
    }

    fn session_id(&self, session: &str) -> Result<String> {
        let mut command = self.command();
        command.args(["list-sessions", "-F", "#{session_name}\t#{session_id}"]);
        let output = self.output(command)?;
        for line in text(&output.stdout).lines() {
            let Some((name, id)) = line.split_once('\t') else {
                continue;
            };
            if name == session {
                return Ok(id.to_owned());
            }
        }
        Err(ToolError::Message(format!(
            "tmux session {session} no longer exists"
        )))
    }

    fn set_option(&self, session: &str, option: &str, value: &str) -> Result<()> {
        let target = self.session_id(session)?;
        let mut command = self.command();
        command
            .args(["set-option", "-t"])
            .arg(target)
            .args([option, value]);
        self.run_command(command)
    }

    fn show_option(&self, session: &str, option: &str) -> Result<String> {
        let target = self.session_id(session)?;
        let mut command = self.command();
        command
            .args(["show-option", "-v", "-t"])
            .arg(target)
            .arg(option);
        let output = self.output(command)?;
        Ok(text(&output.stdout).trim().to_owned())
    }

    fn configure_key_table(&self, session: &str) -> Result<()> {
        let tables = session_key_tables(session);
        let active = self.show_option(session, "key-table")?;
        let active = if active.is_empty() {
            "root".to_owned()
        } else {
            active
        };
        let staging = if active == tables[0] {
            &tables[1]
        } else {
            &tables[0]
        };

        let mut list = self.command();
        list.args(["list-keys", "-T", &active, "-a"]);
        let output = self.output(list)?;
        let source_table = format!("-T {active}");
        let replacement = format!("-T {staging}");
        let mut bindings = String::new();
        for line in text(&output.stdout).lines() {
            bindings.push_str(&line.replacen(&source_table, &replacement, 1));
            bindings.push('\n');
        }
        bindings.push_str(&format!("bind-key -T {staging} C-] detach-client\n"));

        let mut clear = self.command();
        clear.args(["unbind-key", "-q", "-a", "-T", staging]);
        self.run_command(clear)?;

        let directory = env::temp_dir().join(format!(
            ".david-key-table-{}-{}",
            stable_hash(staging),
            std::process::id()
        ));
        let path = directory.join("bindings.conf");
        let mut owns_directory = false;
        let mut owns_file = false;
        let result = (|| {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&directory)?;
            owns_directory = true;

            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&path)?;
            owns_file = true;
            file.write_all(bindings.as_bytes())?;
            let mut source = self.command();
            source.arg("source-file").arg(&path);
            self.run_command(source)
        })();
        let mut cleanup_errors = Vec::new();
        if owns_file && let Err(error) = fs::remove_file(&path) {
            cleanup_errors.push(format!("file: {error}"));
        }
        if owns_directory && let Err(error) = fs::remove_dir(&directory) {
            cleanup_errors.push(format!("directory: {error}"));
        }
        let cleanup = if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(ToolError::Message(format!(
                "failed to remove temporary tmux configuration ({})",
                cleanup_errors.join(", ")
            )))
        };
        match (result, cleanup) {
            (Err(error), Err(cleanup_error)) => {
                return Err(ToolError::Message(format!("{error}; {cleanup_error}")));
            }
            (Err(error), Ok(())) => return Err(error),
            (Ok(()), Err(cleanup_error)) => return Err(cleanup_error),
            (Ok(()), Ok(())) => {}
        }

        self.set_option(session, "key-table", staging)?;
        if tables.iter().any(|table| table == &active) && active != *staging {
            self.clear_key_table(&active)?;
        }
        Ok(())
    }

    fn clear_key_table(&self, table: &str) -> Result<()> {
        let mut command = self.command();
        command.args(["unbind-key", "-q", "-a", "-T", table]);
        self.run_command(command)
    }

    fn clear_key_tables(&self, session: &str) -> Result<()> {
        for table in session_key_tables(session) {
            self.clear_key_table(&table)?;
        }
        Ok(())
    }

    fn session_window_target(&self, name: &str) -> Result<String> {
        let target = exact_session_target(name);
        let mut command = self.command();
        command
            .args(["list-windows", "-F", "#{window_index}", "-t"])
            .arg(target);
        let output = command.output()?;
        if !output.status.success() {
            return Err(command_error("tmux", &output));
        }
        let index = text(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned();
        if index.is_empty() || !index.chars().all(|character| character.is_ascii_digit()) {
            return Err(ToolError::Message(format!(
                "tmux session {name} returned an invalid window index"
            )));
        }
        Ok(format!("{}:{index}", exact_session_target(name)))
    }

    fn pane_metadata(&self, name: &str, pane: &str) -> Result<(String, bool)> {
        if !valid_pane_id(pane) {
            return Err(ToolError::Message(format!(
                "tmux pane target is invalid: {pane}"
            )));
        }
        let mut command = self.command();
        command
            .args(["display-message", "-p", "-t"])
            .arg(pane)
            .arg("#{session_name}|#{window_index}|#{pane_dead}");
        let output = command.output()?;
        if !output.status.success() {
            return Err(command_error("tmux", &output));
        }

        let detail = text(&output.stdout);
        let mut fields = detail.trim().split('|');
        let (Some(session), Some(window), Some(pane_dead)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} returned invalid metadata"
            )));
        };
        if fields.next().is_some() {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} returned invalid metadata"
            )));
        }
        if session != name {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} does not belong to exact session {name}"
            )));
        }
        if window.is_empty() {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} returned an empty window index"
            )));
        }
        let alive = match pane_dead {
            "0" => true,
            "1" => false,
            _ => {
                return Err(ToolError::Message(format!(
                    "tmux pane {pane} returned an invalid liveness value"
                )));
            }
        };
        Ok((window.to_owned(), alive))
    }

    fn delete_buffer(&self, buffer: &str) {
        let mut command = self.command();
        command.args(["delete-buffer", "-b"]).arg(buffer);
        let _ = command.output();
    }

    fn pane_target(&self, name: &str, pane: &str) -> Result<String> {
        let (window, alive) = self.pane_metadata(name, pane)?;
        if !alive {
            return Err(ToolError::Message(format!("tmux pane {pane} is dead")));
        }
        Ok(format!(
            "{}:{}.{}",
            exact_session_target(name),
            window,
            pane
        ))
    }

    fn deliver_prompt_at(&self, pane: &str, message: &str) -> Result<()> {
        let buffer = (!message.is_empty()).then(prompt_buffer_name);
        let mut command = self.command();
        if let Some(buffer) = buffer.as_deref() {
            command
                .args(["load-buffer", "-b"])
                .arg(buffer)
                .args(["-"])
                .arg(";")
                .args(["paste-buffer", "-dprS", "-b"])
                .arg(buffer)
                .args(["-t"])
                .arg(pane)
                .arg(";");
        }
        command.args(["send-keys", "-t"]).arg(pane).arg("Enter");

        let cleanup = || {
            if let Some(buffer) = buffer.as_deref() {
                self.delete_buffer(buffer);
            }
        };
        let mut child = match command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                cleanup();
                return Err(if error.kind() == io::ErrorKind::NotFound {
                    ToolError::Message(
                        "tmux is required but was not found; install tmux and retry".to_owned(),
                    )
                } else {
                    ToolError::Io(error)
                });
            }
        };

        let Some(mut input) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            cleanup();
            return Err(ToolError::Message(
                "tmux prompt transport did not provide stdin".to_owned(),
            ));
        };
        if let Err(error) = input.write_all(message.as_bytes()) {
            drop(input);
            let _ = child.kill();
            let _ = child.wait();
            cleanup();
            return Err(ToolError::Io(error));
        }
        drop(input);

        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                cleanup();
                return Err(ToolError::Io(error));
            }
        };
        if output.status.success() {
            Ok(())
        } else {
            cleanup();
            Err(command_error("tmux", &output))
        }
    }
}

impl SessionBackend for TmuxBackend {
    fn ensure_available(&self) -> Result<()> {
        let output = self.command().arg("-V").output().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ToolError::Message(
                    "tmux is required but was not found; install tmux and retry".to_owned(),
                )
            } else {
                ToolError::Io(error)
            }
        })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_error("tmux", &output))
        }
    }

    fn has_session(&self, name: &str) -> Result<bool> {
        let target = exact_session_target(name);
        let output = self
            .command()
            .args(["has-session", "-t"])
            .arg(target)
            .output()?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) {
            Ok(false)
        } else {
            Err(command_error("tmux", &output))
        }
    }

    fn create_session(&self, name: &str, cwd: &Path, agent: &Agent) -> Result<()> {
        self.create_session_with_pane(name, cwd, agent).map(|_| ())
    }

    fn create_session_with_pane(
        &self,
        name: &str,
        cwd: &Path,
        agent: &Agent,
    ) -> Result<Option<String>> {
        let mut command = self.command();
        command
            .args([
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                name,
                "-c",
            ])
            .arg(cwd)
            .arg("--")
            .arg(&agent.command)
            .args(&agent.args);
        let output = command.output()?;
        if !output.status.success() {
            return Err(command_error("tmux", &output));
        }
        let pane = text(&output.stdout).trim().to_owned();
        if !valid_pane_id(&pane) {
            let _ = self.kill_session(name);
            return Err(ToolError::Message(format!(
                "tmux session {name} returned an invalid pane id"
            )));
        }

        let target = match self.session_window_target(name) {
            Ok(target) => target,
            Err(error) => {
                let _ = self.kill_session(name);
                return Err(error);
            }
        };
        let mut status = self.command();
        status
            .args(["set-option", "-t"])
            .arg(target)
            .args(["status", "off"]);
        let output = match status.output() {
            Ok(output) => output,
            Err(error) => {
                let _ = self.kill_session(name);
                return Err(ToolError::Io(error));
            }
        };
        if output.status.success() {
            Ok(Some(pane))
        } else {
            let _ = self.kill_session(name);
            Err(command_error("tmux", &output))
        }
    }

    fn configure_session(&self, name: &str, metadata: &SessionMetadata) -> Result<()> {
        self.set_option(name, "@david-project", &metadata.project_name)?;
        self.set_option(name, "@david-worktree", &metadata.worktree_name)?;
        self.set_option(name, "@david-agent", &metadata.agent_name)?;
        self.configure_key_table(name)?;
        self.set_option(name, "status", "on")?;
        self.set_option(
            name,
            "status-left",
            "[DAVID] project: #{@david-project} | worktree: #{@david-worktree} | agent: #{@david-agent}",
        )?;
        self.set_option(name, "status-left-length", &status_left_length(metadata))?;
        self.set_option(name, "status-right", "detach: Ctrl-]")?;
        self.set_option(name, "status-right-length", "32")
    }

    fn attach(&self, name: &str) -> Result<()> {
        let target = self.session_id(name)?;
        let mut command = self.command();
        command.args(["attach-session", "-t"]).arg(target);
        let status = command.status()?;
        if status.success() || !self.has_session(name)? {
            Ok(())
        } else {
            Err(self.status_error(status))
        }
    }

    fn agent_pane(&self, name: &str) -> Result<Option<String>> {
        let target = exact_session_target(name);
        let mut command = self.command();
        command
            .args(["list-panes", "-s", "-t"])
            .arg(target)
            .args(["-F", "#{pane_id}|#{session_name}|#{pane_dead}"]);
        let output = command.output()?;
        if !output.status.success() {
            return Err(command_error("tmux", &output));
        }

        let pane_output = text(&output.stdout);
        let line = pane_output
            .lines()
            .next()
            .ok_or_else(|| ToolError::Message(format!("tmux session {name} has no agent pane")))?;
        let mut fields = line.split('|');
        let (Some(pane), Some(session), Some(pane_dead)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(ToolError::Message(format!(
                "tmux session {name} returned invalid pane metadata"
            )));
        };
        if fields.next().is_some() {
            return Err(ToolError::Message(format!(
                "tmux session {name} returned invalid pane metadata"
            )));
        }
        if session != name {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} does not belong to exact session {name}"
            )));
        }
        if !valid_pane_id(pane) {
            return Err(ToolError::Message(format!(
                "tmux session {name} returned an invalid pane id"
            )));
        }
        match pane_dead {
            "0" => {}
            "1" => {
                return Err(ToolError::Message(format!("tmux pane {pane} is dead")));
            }
            _ => {
                return Err(ToolError::Message(format!(
                    "tmux session {name} returned an invalid pane liveness value"
                )));
            }
        }
        Ok(Some(pane.to_owned()))
    }

    fn pane_is_alive(&self, name: &str, pane: &str) -> Result<bool> {
        if !valid_pane_id(pane) {
            return Err(ToolError::Message(format!(
                "tmux pane target is invalid: {pane}"
            )));
        }
        let mut command = self.command();
        command
            .args(["display-message", "-p", "-t"])
            .arg(pane)
            .arg("#{session_name}|#{pane_dead}");
        let output = command.output()?;
        if !output.status.success() {
            return if output.status.code() == Some(1) {
                Ok(false)
            } else {
                Err(command_error("tmux", &output))
            };
        }

        let detail = text(&output.stdout);
        let mut fields = detail.trim().split('|');
        let (Some(session), Some(pane_dead)) = (fields.next(), fields.next()) else {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} returned invalid liveness metadata"
            )));
        };
        if fields.next().is_some() {
            return Err(ToolError::Message(format!(
                "tmux pane {pane} returned invalid liveness metadata"
            )));
        }
        if session != name {
            return Ok(false);
        }
        match pane_dead {
            "0" => Ok(true),
            "1" => Ok(false),
            _ => Err(ToolError::Message(format!(
                "tmux pane {pane} returned an invalid liveness value"
            ))),
        }
    }

    fn deliver_prompt(&self, name: &str, message: &str) -> Result<()> {
        let pane = self
            .agent_pane(name)?
            .ok_or_else(|| ToolError::Message(format!("tmux session {name} has no agent pane")))?;
        let target = self.pane_target(name, &pane)?;
        self.deliver_prompt_at(&target, message)
    }

    fn deliver_prompt_to(&self, name: &str, message: &str, pane: Option<&str>) -> Result<()> {
        let Some(pane) = pane else {
            return self.deliver_prompt(name, message);
        };
        let target = self.pane_target(name, pane)?;
        self.deliver_prompt_at(&target, message)
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        if !self.has_session(name)? {
            return self.clear_key_tables(name);
        }
        let target = self.session_id(name)?;
        let mut command = self.command();
        command.args(["kill-session", "-t"]).arg(target);
        let kill_result = self.output(command);
        let cleanup_result = self.clear_key_tables(name);
        match (kill_result, cleanup_result) {
            (Ok(_), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(kill_error), Err(cleanup_error)) => Err(ToolError::Message(format!(
                "{kill_error}; failed to clean up session key tables: {cleanup_error}"
            ))),
        }
    }
}

pub struct App<S, P = TerminalAgentPicker> {
    paths: DavidPaths,
    git: Git,
    sessions: S,
    picker: P,
}

impl<S: SessionBackend> App<S, TerminalAgentPicker> {
    pub fn new(paths: DavidPaths, sessions: S) -> Self {
        Self::with_picker(paths, sessions, TerminalAgentPicker)
    }
}

impl<S: SessionBackend, P: AgentPicker> App<S, P> {
    pub fn with_picker(paths: DavidPaths, sessions: S, picker: P) -> Self {
        Self {
            paths,
            git: Git::default(),
            sessions,
            picker,
        }
    }

    pub fn run(&self, cwd: &Path, name: &str) -> Result<()> {
        self.sessions.ensure_available()?;
        validate_worktree_name(name)?;

        let root = self.git.repository_root(cwd)?;
        let repo_id = self.git.repository_id(&root)?;
        let project_name = self.git.repository_name(&root)?;
        let target = self.paths.worktree_path(&repo_id, name);
        self.paths.validate_worktree_path(&repo_id, name)?;
        let existing = self.find_worktree(&root, &target)?;

        if target.exists() && existing.is_none() {
            return Err(ToolError::Message(format!(
                "managed worktree path already exists but is not a Git worktree: {}",
                target.display()
            )));
        }
        let creating = existing.is_none();
        if let Some(worktree) = existing.as_ref()
            && worktree.branch.as_deref() != Some(name)
        {
            return Err(ToolError::Message(format!(
                "managed worktree {name} is not attached to its expected branch"
            )));
        }
        if creating {
            self.git.current_head(&root)?;
            if self.git.source_is_dirty(&root)? {
                return Err(ToolError::Message(
                    "source repository has uncommitted changes; commit or stash them first"
                        .to_owned(),
                ));
            }
            self.git.validate_branch(&root, name)?;
        }

        let session = session_name(&repo_id, name);
        let state_path = self.paths.session_state_path(&repo_id, name);
        let live = self.sessions.has_session(&session)?;
        if live {
            let state = if state_path.is_file() {
                read_session_state(&state_path)?
            } else {
                return Err(ToolError::Message(format!(
                    "tmux session {session} exists but is not managed by david"
                )));
            };
            if !state.matches(&repo_id, name, &target, &session) {
                return Err(ToolError::Message(format!(
                    "tmux session {session} metadata does not match the requested worktree"
                )));
            }
            let metadata = SessionMetadata {
                project_name: project_name.clone(),
                worktree_name: name.to_owned(),
                agent_name: state.agent,
            };
            self.sessions.configure_session(&session, &metadata)?;
            return self.sessions.attach(&session);
        }
        if state_path.exists() {
            let state = read_session_state(&state_path)?;
            if !state.matches(&repo_id, name, &target, &session) {
                return Err(ToolError::Message(format!(
                    "session metadata does not match the requested worktree {name}"
                )));
            }
            fs::remove_file(&state_path)?;
        }

        let config = Config::load(self.paths.config_path())?;
        let (agent_name, agent) = self.picker.pick(&config)?;
        if !command_available(&agent.command) {
            return Err(ToolError::Message(format!(
                "configured agent command is not available: {}",
                agent.command
            )));
        }

        if creating {
            self.paths.prepare()?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            self.git.add_worktree(&root, name, &target)?;
        }

        self.paths.prepare()?;
        let metadata = SessionMetadata {
            project_name,
            worktree_name: name.to_owned(),
            agent_name: agent_name.clone(),
        };
        let mut state = SessionState {
            repo_id: repo_id.clone(),
            worktree_name: name.to_owned(),
            worktree_path: target.clone(),
            branch: name.to_owned(),
            session: session.clone(),
            agent: agent_name,
            pane: None,
        };
        write_session_state(&state_path, &state)?;
        let pane = match self
            .sessions
            .create_session_with_pane(&session, &target, &agent)
        {
            Ok(pane) => pane,
            Err(error) => {
                self.cleanup_failed_session(&state_path, &session);
                return Err(error);
            }
        };

        let live = match self.sessions.has_session(&session) {
            Ok(live) => live,
            Err(error) => {
                self.cleanup_failed_session(&state_path, &session);
                return Err(error);
            }
        };
        if !live {
            let _ = fs::remove_file(&state_path);
            return Err(ToolError::Message(format!(
                "agent session {session} exited before it could be attached"
            )));
        }
        if let Some(pane) = pane.as_deref() {
            let alive = match self.sessions.pane_is_alive(&session, pane) {
                Ok(alive) => alive,
                Err(error) => {
                    self.cleanup_failed_session(&state_path, &session);
                    return Err(error);
                }
            };
            if !alive {
                self.cleanup_failed_session(&state_path, &session);
                return Err(ToolError::Message(format!(
                    "agent pane {pane} in session {session} is dead"
                )));
            }
        }
        state.pane = pane;
        if let Err(error) = write_session_state(&state_path, &state) {
            self.cleanup_failed_session(&state_path, &session);
            return Err(error);
        }
        if !self.sessions.has_session(&session)? {
            fs::remove_file(&state_path)?;
            return Err(ToolError::Message(format!(
                "agent session {session} exited before it could be attached"
            )));
        }
        if let Err(error) = self.sessions.configure_session(&session, &metadata) {
            let alive = match self.sessions.has_session(&session) {
                Ok(alive) => alive,
                Err(check_error) => {
                    return Err(ToolError::Message(format!(
                        "{error}; could not verify agent session cleanup: {check_error}"
                    )));
                }
            };
            if alive && let Err(cleanup_error) = self.sessions.kill_session(&session) {
                return Err(ToolError::Message(format!(
                    "{error}; failed to clean up agent session: {cleanup_error}"
                )));
            }
            if let Err(state_error) = fs::remove_file(&state_path) {
                return Err(ToolError::Message(format!(
                    "{error}; failed to remove session metadata: {state_error}"
                )));
            }
            if !alive {
                return Err(ToolError::Message(format!(
                    "agent session {session} exited before it could be attached"
                )));
            }
            return Err(error);
        }
        if !self.sessions.has_session(&session)? {
            fs::remove_file(&state_path)?;

            return Err(ToolError::Message(format!(
                "agent session {session} exited before it could be attached"
            )));
        }
        self.sessions.attach(&session)
    }

    pub fn prompt(&self, cwd: &Path, name: &str, message: &str) -> Result<()> {
        self.sessions.ensure_available()?;
        validate_worktree_name(name)?;

        let root = self.git.repository_root(cwd)?;
        let repo_id = self.git.repository_id(&root)?;
        let target = self.paths.worktree_path(&repo_id, name);
        self.paths.validate_worktree_path(&repo_id, name)?;
        if !target.is_dir() {
            return Err(ToolError::Message(format!(
                "managed worktree does not exist: {name}"
            )));
        }
        let worktree = self.find_worktree(&root, &target)?.ok_or_else(|| {
            ToolError::Message(format!("managed worktree does not exist: {name}"))
        })?;
        if worktree.branch.as_deref() != Some(name) {
            return Err(ToolError::Message(format!(
                "managed worktree {name} is not attached to its expected branch"
            )));
        }

        let session = session_name(&repo_id, name);
        let state_path = self.paths.session_state_path(&repo_id, name);
        let live = self.sessions.has_session(&session)?;
        if !state_path.is_file() {
            return Err(if live {
                ToolError::Message(format!(
                    "agent session {session} exists but is not managed by david"
                ))
            } else {
                ToolError::Message(format!(
                    "managed agent session {session} is missing or not running"
                ))
            });
        }

        let state = read_session_state(&state_path)?;
        if !state.matches(&repo_id, name, &target, &session) {
            return Err(ToolError::Message(format!(
                "agent session {session} metadata does not match the requested worktree"
            )));
        }
        if !live {
            return Err(ToolError::Message(format!(
                "managed agent session {session} is not running"
            )));
        }

        let pane = match state.pane {
            Some(pane) => Some(pane),
            None => self.sessions.agent_pane(&session)?,
        };
        if let Some(pane) = pane.as_deref()
            && !self.sessions.pane_is_alive(&session, pane)?
        {
            return Err(ToolError::Message(format!(
                "agent pane {pane} in session {session} is dead"
            )));
        }
        self.sessions
            .deliver_prompt_to(&session, message, pane.as_deref())
            .map_err(|error| {
                ToolError::Message(format!(
                    "failed to deliver prompt to agent session {session}: {error}"
                ))
            })
    }

    pub fn list<W: Write>(&self, cwd: &Path, output: &mut W) -> Result<()> {
        let entries = self.managed_worktrees(cwd)?;

        writeln!(output, "NAME\tBRANCH\tAGENT\tPATH")?;
        for entry in &entries {
            writeln!(
                output,
                "{}\t{}\t{}\t{}",
                entry.name,
                entry.branch,
                entry.agent,
                entry.path.display()
            )?;
        }
        if entries.is_empty() {
            writeln!(output, "No managed worktrees.")?;
        }
        Ok(())
    }

    pub fn list_porcelain<W: Write>(&self, cwd: &Path, zero: bool, output: &mut W) -> Result<()> {
        let entries = self.managed_worktrees(cwd)?;
        write_porcelain_list(&entries, zero, output)
    }

    pub fn path<W: Write>(&self, cwd: &Path, name: &str, zero: bool, output: &mut W) -> Result<()> {
        validate_worktree_name(name)?;

        let root = self.git.repository_root(cwd)?;
        let repo_id = self.git.repository_id(&root)?;
        let target = self.paths.worktree_path(&repo_id, name);
        self.paths.validate_worktree_path(&repo_id, name)?;
        let worktree = self.find_worktree(&root, &target)?.ok_or_else(|| {
            ToolError::Message(format!("managed worktree does not exist: {name}"))
        })?;
        if worktree.branch.as_deref() != Some(name) {
            return Err(ToolError::Message(format!(
                "managed worktree {name} is not attached to its expected branch"
            )));
        }

        let path = fs::canonicalize(&worktree.path)?;
        #[cfg(unix)]
        output.write_all(path.as_os_str().as_bytes())?;
        #[cfg(not(unix))]
        output.write_all(path.to_string_lossy().as_bytes())?;
        output.write_all(&[if zero { b'\0' } else { b'\n' }])?;
        Ok(())
    }

    fn managed_worktrees(&self, cwd: &Path) -> Result<Vec<ManagedWorktree>> {
        self.sessions.ensure_available()?;
        let root = self.git.repository_root(cwd)?;
        let repo_id = self.git.repository_id(&root)?;
        let base = self.paths.repository_worktrees(&repo_id);
        let base = fs::canonicalize(&base).unwrap_or(base);
        let worktrees = self.git.worktrees(&root)?;
        let mut entries = Vec::new();

        for worktree in worktrees {
            let Some(relative) = worktree.path.strip_prefix(&base).ok() else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let name = relative.to_string_lossy().to_string();
            if self.paths.validate_worktree_path(&repo_id, &name).is_err() {
                continue;
            }
            let (agent, session) = self.session_status(&repo_id, &name, &worktree.path)?;
            entries.push(ManagedWorktree {
                name,
                branch: worktree
                    .branch
                    .as_deref()
                    .unwrap_or("(detached)")
                    .to_owned(),
                agent,
                session,
                path: worktree.path,
            });
        }
        Ok(entries)
    }

    fn session_status(
        &self,
        repo_id: &str,
        name: &str,
        worktree_path: &Path,
    ) -> Result<(String, SessionStatus)> {
        let session = session_name(repo_id, name);
        let state_path = self.paths.session_state_path(repo_id, name);
        if !state_path.is_file() {
            return Ok(("-".to_owned(), SessionStatus::Unknown));
        }
        if !self.sessions.has_session(&session)? {
            return Ok(("-".to_owned(), SessionStatus::Inactive));
        }

        let state = read_session_state(&state_path)?;
        if !state.matches(repo_id, name, worktree_path, &session) {
            return Err(ToolError::Message(format!(
                "session metadata does not match managed worktree {name}"
            )));
        }
        let pane_alive = if let Some(pane) = state.pane.as_deref() {
            self.sessions
                .pane_is_alive(&session, pane)
                .unwrap_or_default()
        } else {
            match self.sessions.agent_pane(&session) {
                Ok(Some(pane)) => self
                    .sessions
                    .pane_is_alive(&session, &pane)
                    .unwrap_or_default(),
                Ok(None) | Err(_) => false,
            }
        };
        let agent = if pane_alive {
            state.agent
        } else {
            "-".to_owned()
        };
        Ok((agent, SessionStatus::Active))
    }

    pub fn remove(&self, cwd: &Path, name: &str, force: bool) -> Result<()> {
        self.sessions.ensure_available()?;
        validate_worktree_name(name)?;

        let root = self.git.repository_root(cwd)?;
        let repo_id = self.git.repository_id(&root)?;
        let target = self.paths.worktree_path(&repo_id, name);
        self.paths.validate_worktree_path(&repo_id, name)?;
        let worktree = self.find_worktree(&root, &target)?.ok_or_else(|| {
            ToolError::Message(format!("managed worktree does not exist: {name}"))
        })?;
        if worktree.branch.as_deref() != Some(name) {
            return Err(ToolError::Message(format!(
                "managed worktree {name} is not attached to its expected branch"
            )));
        }

        if !force && self.git.worktree_is_dirty(&target)? {
            return Err(ToolError::Message(format!(
                "worktree {name} has uncommitted changes; use --force to remove it"
            )));
        }

        let session = session_name(&repo_id, name);
        let state_path = self.paths.session_state_path(&repo_id, name);
        let live = self.sessions.has_session(&session)?;
        if state_path.is_file() {
            let state = read_session_state(&state_path)?;
            if !state.matches(&repo_id, name, &target, &session) {
                return Err(ToolError::Message(format!(
                    "session metadata does not match managed worktree {name}"
                )));
            }
        } else if live {
            return Err(ToolError::Message(format!(
                "tmux session {session} exists but is not managed by david"
            )));
        }
        if live {
            self.sessions.kill_session(&session)?;
            if !force && self.git.worktree_is_dirty(&target)? {
                return Err(ToolError::Message(format!(
                    "worktree {name} changed while its agent was stopping; the session is stopped but the worktree was not removed"
                )));
            }
        }
        let branch_root = self.git.branch_command_root(&root, &target)?;
        self.git.remove_worktree(&root, &target, force)?;
        self.git.delete_branch(&branch_root, name)?;
        if state_path.exists() {
            fs::remove_file(state_path)?;
        }
        Ok(())
    }

    fn cleanup_failed_session(&self, state_path: &Path, session: &str) {
        let _ = fs::remove_file(state_path);
        let _ = self.sessions.kill_session(session);
    }

    fn find_worktree(&self, root: &Path, target: &Path) -> Result<Option<Worktree>> {
        let expected = fs::canonicalize(target).ok();
        for worktree in self.git.worktrees(root)? {
            let actual = fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
            if expected.as_ref() == Some(&actual) || worktree.path == target {
                return Ok(Some(worktree));
            }
        }
        Ok(None)
    }
}

pub fn validate_worktree_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.starts_with('-') || name.contains('\0') {
        return Err(ToolError::Message(
            "worktree name must be a non-empty value that does not start with '-'".to_owned(),
        ));
    }
    let path = Path::new(name);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ToolError::Message(
            "worktree name must not escape the managed directory".to_owned(),
        ));
    }
    Ok(())
}

pub fn repository_id(root: &Path) -> Result<String> {
    let canonical = fs::canonicalize(root)?;
    let identity_path = if canonical.file_name().and_then(|value| value.to_str()) == Some(".git") {
        canonical.parent().unwrap_or(&canonical)
    } else {
        &canonical
    };
    let name = identity_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(slug)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repo".to_owned());
    Ok(format!(
        "{name}-{}",
        stable_hash(&canonical.to_string_lossy())
    ))
}

pub fn stable_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn session_name(repo_id: &str, worktree_name: &str) -> String {
    format!("david-{repo_id}-{}", stable_hash(worktree_name))
}

fn exact_session_target(session: &str) -> String {
    format!("={session}")
}

fn session_key_tables(session: &str) -> [String; 2] {
    let base = format!("david-keys-{}", stable_hash(session));
    [format!("{base}-a"), format!("{base}-b")]
}

fn status_left_length(metadata: &SessionMetadata) -> String {
    let length = "[DAVID] project:  | worktree:  | agent: ".chars().count()
        + metadata.project_name.chars().count()
        + metadata.worktree_name.chars().count()
        + metadata.agent_name.chars().count();
    length.to_string()
}

static NEXT_PROMPT_BUFFER_ID: AtomicU64 = AtomicU64::new(0);

fn prompt_buffer_name() -> String {
    format!(
        "david-prompt-{}-{}",
        std::process::id(),
        NEXT_PROMPT_BUFFER_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_config(path: &Path, config: &Config) -> Result<()> {
    let temporary = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml"),
        std::process::id()
    ));
    let content = toml::to_string_pretty(config)?;
    fs::write(&temporary, content)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(ToolError::Io(error));
    }
    Ok(())
}

fn write_session_state(path: &Path, state: &SessionState) -> Result<()> {
    let temporary = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id()
    ));
    fs::write(&temporary, state.encode())?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(ToolError::Io(error));
    }
    Ok(())
}

fn read_session_state(path: &Path) -> Result<SessionState> {
    let content = fs::read_to_string(path)?;
    let mut values = BTreeMap::new();
    for line in content.lines() {
        let Some((key, value)) = line.split_once('=') else {
            return Err(ToolError::Message(format!(
                "invalid session metadata: {}",
                path.display()
            )));
        };
        if !matches!(
            key,
            "repo_id"
                | "worktree_name"
                | "worktree_path"
                | "worktree_path_hex"
                | "branch"
                | "session"
                | "agent"
                | "pane"
        ) {
            return Err(ToolError::Message(format!(
                "session metadata contains unknown field {key}: {}",
                path.display()
            )));
        }
        if values.insert(key, value.to_owned()).is_some() {
            return Err(ToolError::Message(format!(
                "session metadata contains duplicate field {key}: {}",
                path.display()
            )));
        }
    }
    let take = |key: &str| {
        values.get(key).cloned().ok_or_else(|| {
            ToolError::Message(format!(
                "session metadata is missing {key}: {}",
                path.display()
            ))
        })
    };
    let agent = take("agent")?;
    if agent.trim().is_empty()
        || agent.contains('\n')
        || agent.contains('\r')
        || agent.contains('\0')
    {
        return Err(ToolError::Message(format!(
            "session metadata has an invalid agent: {}",
            path.display()
        )));
    }
    let worktree_path = if let Some(encoded) = values.get("worktree_path_hex") {
        if values.contains_key("worktree_path") {
            return Err(ToolError::Message(format!(
                "session metadata contains duplicate worktree path fields: {}",
                path.display()
            )));
        }
        decode_path_hex(encoded).ok_or_else(|| {
            ToolError::Message(format!(
                "session metadata has an invalid worktree path: {}",
                path.display()
            ))
        })?
    } else {
        PathBuf::from(take("worktree_path")?)
    };
    let pane = values.get("pane").cloned();
    if let Some(pane) = &pane
        && !valid_pane_id(pane)
    {
        return Err(ToolError::Message(format!(
            "session metadata has an invalid pane: {}",
            path.display()
        )));
    }
    Ok(SessionState {
        repo_id: take("repo_id")?,
        worktree_name: take("worktree_name")?,
        worktree_path,
        branch: take("branch")?,
        session: take("session")?,
        agent,
        pane,
    })
}

fn canonicalize_with_missing(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    let mut missing = Vec::new();

    loop {
        match fs::symlink_metadata(&current) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(&current)?;
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(file_name) = current.file_name() else {
                    return Err(error);
                };
                missing.push(file_name.to_os_string());
                current = current
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(|parent| parent.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
            }
            Err(error) => return Err(error),
        }
    }
}

fn same_path(first: &Path, second: &Path) -> bool {
    let first = fs::canonicalize(first).unwrap_or_else(|_| first.to_path_buf());
    let second = fs::canonicalize(second).unwrap_or_else(|_| second.to_path_buf());
    first == second
}

fn valid_pane_id(value: &str) -> bool {
    value
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.chars().all(|character| character.is_ascii_digit()))
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn command_available(command: &str) -> bool {
    let candidate = Path::new(command);
    if candidate.is_absolute() || candidate.components().count() > 1 {
        return candidate.is_file();
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| directory.join(command).is_file())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn command_error(program: &str, output: &Output) -> ToolError {
    let detail = text(&output.stderr).trim().to_owned();
    let detail = if detail.is_empty() {
        let stdout = text(&output.stdout).trim().to_owned();
        if stdout.is_empty() {
            format!("exited with status {}", output.status)
        } else {
            stdout
        }
    } else {
        detail
    };
    ToolError::Command {
        program: program.to_owned(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeSet, process::Command, rc::Rc};
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct FakeSessions {
        state: Rc<RefCell<FakeSessionState>>,
    }

    #[derive(Default)]
    struct FakeSessionState {
        live: BTreeSet<String>,
        created: Vec<String>,
        configured: Vec<(String, SessionMetadata)>,
        attached: Vec<String>,
        killed: Vec<String>,
        configure_error: Option<String>,
        kill_error: Option<String>,
        deliveries: Vec<(String, String)>,
        prompt_error: Option<String>,
    }

    impl SessionBackend for FakeSessions {
        fn ensure_available(&self) -> Result<()> {
            Ok(())
        }

        fn has_session(&self, name: &str) -> Result<bool> {
            Ok(self.state.borrow().live.contains(name))
        }

        fn create_session(&self, name: &str, _cwd: &Path, _agent: &Agent) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.live.insert(name.to_owned());
            state.created.push(name.to_owned());
            Ok(())
        }

        fn configure_session(&self, name: &str, metadata: &SessionMetadata) -> Result<()> {
            let mut state = self.state.borrow_mut();
            if let Some(message) = &state.configure_error {
                return Err(ToolError::Message(message.clone()));
            }
            state.configured.push((name.to_owned(), metadata.clone()));
            Ok(())
        }

        fn agent_pane(&self, _name: &str) -> Result<Option<String>> {
            Ok(Some("%0".to_owned()))
        }

        fn attach(&self, name: &str) -> Result<()> {
            self.state.borrow_mut().attached.push(name.to_owned());
            Ok(())
        }

        fn deliver_prompt(&self, name: &str, message: &str) -> Result<()> {
            let mut state = self.state.borrow_mut();
            if let Some(error) = state.prompt_error.as_ref() {
                return Err(ToolError::Message(error.clone()));
            }
            state.deliveries.push((name.to_owned(), message.to_owned()));
            Ok(())
        }

        fn kill_session(&self, name: &str) -> Result<()> {
            let mut state = self.state.borrow_mut();
            if let Some(message) = &state.kill_error {
                return Err(ToolError::Message(message.clone()));
            }
            state.live.remove(name);
            state.killed.push(name.to_owned());
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct PaneSessions {
        state: Rc<RefCell<PaneSessionState>>,
    }

    #[derive(Default)]
    struct PaneSessionState {
        live: BTreeSet<String>,
        pane: Option<String>,
        pane_error: Option<String>,
        pane_dead: bool,
        deliveries: Vec<(String, String, Option<String>)>,
        killed: Vec<String>,
    }

    impl SessionBackend for PaneSessions {
        fn ensure_available(&self) -> Result<()> {
            Ok(())
        }

        fn has_session(&self, name: &str) -> Result<bool> {
            Ok(self.state.borrow().live.contains(name))
        }

        fn create_session(&self, name: &str, _cwd: &Path, _agent: &Agent) -> Result<()> {
            self.state.borrow_mut().live.insert(name.to_owned());
            Ok(())
        }

        fn agent_pane(&self, _name: &str) -> Result<Option<String>> {
            let state = self.state.borrow();
            if let Some(error) = state.pane_error.as_ref() {
                return Err(ToolError::Message(error.clone()));
            }
            Ok(state.pane.clone())
        }

        fn pane_is_alive(&self, _name: &str, _pane: &str) -> Result<bool> {
            Ok(!self.state.borrow().pane_dead)
        }

        fn attach(&self, _name: &str) -> Result<()> {
            Ok(())
        }

        fn deliver_prompt_to(&self, name: &str, message: &str, pane: Option<&str>) -> Result<()> {
            self.state.borrow_mut().deliveries.push((
                name.to_owned(),
                message.to_owned(),
                pane.map(str::to_owned),
            ));
            Ok(())
        }

        fn kill_session(&self, name: &str) -> Result<()> {
            let mut state = self.state.borrow_mut();
            state.live.remove(name);
            state.killed.push(name.to_owned());
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct AtomicPaneSessions {
        inner: PaneSessions,
    }

    impl SessionBackend for AtomicPaneSessions {
        fn ensure_available(&self) -> Result<()> {
            self.inner.ensure_available()
        }

        fn has_session(&self, name: &str) -> Result<bool> {
            self.inner.has_session(name)
        }

        fn create_session(&self, name: &str, cwd: &Path, agent: &Agent) -> Result<()> {
            self.inner.create_session(name, cwd, agent)
        }

        fn create_session_with_pane(
            &self,
            name: &str,
            cwd: &Path,
            agent: &Agent,
        ) -> Result<Option<String>> {
            self.inner.create_session(name, cwd, agent)?;
            Ok(Some("%99".to_owned()))
        }

        fn agent_pane(&self, _name: &str) -> Result<Option<String>> {
            Err(ToolError::Message(
                "agent pane was queried after session creation".to_owned(),
            ))
        }

        fn pane_is_alive(&self, name: &str, pane: &str) -> Result<bool> {
            self.inner.pane_is_alive(name, pane)
        }

        fn attach(&self, name: &str) -> Result<()> {
            self.inner.attach(name)
        }

        fn deliver_prompt_to(&self, name: &str, message: &str, pane: Option<&str>) -> Result<()> {
            self.inner.deliver_prompt_to(name, message, pane)
        }

        fn kill_session(&self, name: &str) -> Result<()> {
            self.inner.kill_session(name)
        }
    }

    #[derive(Clone, Copy, Default)]
    struct FirstAgentPicker;

    impl AgentPicker for FirstAgentPicker {
        fn pick(&self, config: &Config) -> Result<(String, Agent)> {
            config
                .agents
                .iter()
                .next()
                .map(|(name, agent)| (name.clone(), agent.clone()))
                .ok_or_else(|| ToolError::Message("no agents configured".to_owned()))
        }
    }

    fn test_app(paths: DavidPaths, sessions: FakeSessions) -> App<FakeSessions, FirstAgentPicker> {
        App::with_picker(paths, sessions, FirstAgentPicker)
    }

    struct ScriptedSetup {
        additions: Vec<(String, Agent)>,
    }

    impl SetupPrompter for ScriptedSetup {
        fn collect(&self, mut config: Config) -> Result<Config> {
            for (name, agent) in &self.additions {
                config.add_or_replace(name.clone(), agent.clone())?;
            }
            Ok(config)
        }
    }

    struct EmptySetup;

    impl SetupPrompter for EmptySetup {
        fn collect(&self, _config: Config) -> Result<Config> {
            Ok(Config::default())
        }
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

    fn configured_paths(home: &Path) -> DavidPaths {
        let paths = DavidPaths::from_home(home);
        fs::create_dir_all(paths.config_path().parent().unwrap()).unwrap();
        fs::write(
            paths.config_path(),
            "[agents.test]\ncommand = \"echo\"\nargs = [\"ready\"]\n",
        )
        .unwrap();
        paths
    }

    #[test]
    fn repository_ids_are_stable_and_distinguish_paths() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert_eq!(
            repository_id(first.path()).unwrap(),
            repository_id(first.path()).unwrap()
        );
        assert_ne!(
            repository_id(first.path()).unwrap(),
            repository_id(second.path()).unwrap()
        );
    }

    #[test]
    fn uses_david_storage_namespace() {
        let home = tempfile::tempdir().unwrap();
        let paths = DavidPaths::from_home(home.path());
        let expected = home.path().join(".david/config.toml");

        assert_eq!(paths.config_path(), expected.as_path());
    }

    #[test]
    fn names_sessions_in_david_namespace() {
        assert!(session_name("repo", "worktree").starts_with("david-"));
    }

    #[test]
    fn session_state_round_trips_pane_and_reads_legacy_state_without_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.state");
        let state = SessionState {
            repo_id: "repo".to_owned(),
            worktree_name: "feature".to_owned(),
            worktree_path: PathBuf::from("/tmp/feature"),
            branch: "feature".to_owned(),
            session: "david-repo-feature".to_owned(),
            agent: "test".to_owned(),
            pane: Some("%42".to_owned()),
        };

        write_session_state(&path, &state).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("pane=%42\n"));
        assert_eq!(read_session_state(&path).unwrap(), state);

        let legacy = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("pane="))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&path, legacy).unwrap();
        assert_eq!(read_session_state(&path).unwrap().pane, None);
    }

    #[cfg(unix)]
    #[test]
    fn session_state_hex_encodes_paths_and_reads_legacy_paths() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.state");
        let worktree_path = PathBuf::from(OsString::from_vec(
            b"/tmp/feature\nwith-newline-\xff".to_vec(),
        ));
        let state = SessionState {
            repo_id: "repo".to_owned(),
            worktree_name: "feature".to_owned(),
            worktree_path,
            branch: "feature".to_owned(),
            session: "david-repo-feature".to_owned(),
            agent: "test".to_owned(),
            pane: None,
        };

        write_session_state(&path, &state).unwrap();
        let encoded = fs::read_to_string(&path).unwrap();
        assert!(encoded.contains(
            "worktree_path_hex=2f746d702f666561747572650a776974682d6e65776c696e652dff\n"
        ));
        assert!(!encoded.contains("worktree_path=/tmp"));
        assert_eq!(read_session_state(&path).unwrap(), state);

        fs::write(
            &path,
            "repo_id=repo\nworktree_name=feature\nworktree_path=/tmp/legacy\nbranch=feature\nsession=david-repo-feature\nagent=test\n",
        )
        .unwrap();
        assert_eq!(
            read_session_state(&path).unwrap().worktree_path,
            PathBuf::from("/tmp/legacy")
        );
    }

    #[test]
    fn rejects_nul_in_agent_configuration_and_session_state_values() {
        let config = |name: &str, agent: Agent| Config {
            agents: BTreeMap::from([(name.to_owned(), agent)]),
        };
        assert!(
            config(
                "bad\0name",
                Agent {
                    command: "echo".to_owned(),
                    args: vec![],
                }
            )
            .validate()
            .is_err()
        );
        assert!(
            config(
                "test",
                Agent {
                    command: "ec\0ho".to_owned(),
                    args: vec![],
                }
            )
            .validate()
            .is_err()
        );
        assert_eq!(
            config(
                "test",
                Agent {
                    command: "echo".to_owned(),
                    args: vec!["bad\0arg".to_owned()],
                }
            )
            .validate()
            .unwrap_err()
            .to_string(),
            "agent \"test\" arguments must not contain NUL bytes"
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.state");
        fs::write(
            &path,
            "repo_id=repo\nworktree_name=feature\nworktree_path=/tmp/feature\nbranch=feature\nsession=david-repo-feature\nagent=bad\0agent\n",
        )
        .unwrap();
        assert!(read_session_state(&path).is_err());
    }

    #[test]
    fn parses_porcelain_worktree_output() {
        let worktrees = parse_worktree_list(
            "worktree /tmp/main\nHEAD abc\nbranch refs/heads/main\n\nworktree /tmp/feature\nHEAD def\nbranch refs/heads/feature\n",
        );
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(worktrees[1].head, "def");
    }

    #[test]
    fn parses_nul_porcelain_worktree_output_without_splitting_newline_paths() {
        let input = b"worktree /tmp/feature\nwith-newline\0HEAD def\0branch refs/heads/feature\0\0";

        let worktrees = parse_worktree_list_bytes(input);

        assert_eq!(worktrees.len(), 1);
        assert_eq!(
            worktrees[0].path,
            PathBuf::from("/tmp/feature\nwith-newline")
        );
        assert_eq!(worktrees[0].branch.as_deref(), Some("feature"));
    }

    #[test]
    fn porcelain_formatter_uses_exact_newline_and_nul_record_delimiters() {
        let entries = vec![
            ManagedWorktree {
                name: "first".to_owned(),
                branch: "main".to_owned(),
                agent: "codex".to_owned(),
                session: SessionStatus::Active,
                path: PathBuf::from("/tmp/first"),
            },
            ManagedWorktree {
                name: "second".to_owned(),
                branch: "(detached)".to_owned(),
                agent: "-".to_owned(),
                session: SessionStatus::Unknown,
                path: PathBuf::from("/tmp/second\nwith-newline"),
            },
        ];

        let mut newline = Vec::new();
        write_porcelain_list(&entries, false, &mut newline).unwrap();
        assert_eq!(
            String::from_utf8(newline).unwrap(),
            "name first\nbranch main\nagent codex\nsession active\npath /tmp/first\n\nname second\nbranch (detached)\nagent -\nsession unknown\npath /tmp/second\nwith-newline\n"
        );

        let mut nul = Vec::new();
        write_porcelain_list(&entries, true, &mut nul).unwrap();
        assert_eq!(
            nul,
            b"name first\0branch main\0agent codex\0session active\0path /tmp/first\0\0name second\0branch (detached)\0agent -\0session unknown\0path /tmp/second\nwith-newline\0"
        );

        let mut empty = Vec::new();
        write_porcelain_list(&[], false, &mut empty).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn rejects_worktree_path_escape() {
        assert!(validate_worktree_name("../escape").is_err());
        assert!(validate_worktree_name("/absolute").is_err());
        assert!(validate_worktree_name("feature/login").is_ok());
    }

    #[test]
    fn parses_quoted_agent_arguments() {
        assert_eq!(
            parse_agent_arguments("--model gpt-5 --prompt 'hello world'").unwrap(),
            vec!["--model", "gpt-5", "--prompt", "hello world"]
        );
    }

    #[test]
    fn setup_merges_agents_and_scaffolds_config() {
        let home = tempfile::tempdir().unwrap();
        let paths = DavidPaths::from_home(home.path());
        fs::create_dir_all(paths.config_path().parent().unwrap()).unwrap();
        fs::write(
            paths.config_path(),
            "[agents.keep]\ncommand = \"keep\"\nargs = []\n\n[agents.existing]\ncommand = \"old\"\nargs = [\"old\"]\n",
        )
        .unwrap();

        paths
            .setup_with(ScriptedSetup {
                additions: vec![
                    (
                        "existing".to_owned(),
                        Agent {
                            command: "new".to_owned(),
                            args: vec!["value".to_owned()],
                        },
                    ),
                    (
                        "added".to_owned(),
                        Agent {
                            command: "added".to_owned(),
                            args: vec![],
                        },
                    ),
                ],
            })
            .unwrap();

        assert!(paths.config_path().is_file());
        assert!(paths.worktrees.is_dir());
        assert!(paths.sessions.is_dir());
        let config = Config::load(paths.config_path()).unwrap();
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.agents["keep"].command, "keep");
        assert_eq!(config.agents["existing"].command, "new");
        assert_eq!(config.agents["existing"].args, vec!["value"]);
        assert_eq!(config.agents["added"].command, "added");
    }

    #[test]
    fn setup_rejects_empty_result_without_writing_config() {
        let home = tempfile::tempdir().unwrap();
        let paths = DavidPaths::from_home(home.path());

        let error = paths.setup_with(EmptySetup).unwrap_err();

        assert!(error.to_string().contains("at least one agent"));
        assert!(!paths.config_path().exists());
        assert!(!paths.worktrees.exists());
        assert!(!paths.sessions.exists());
    }

    #[test]
    fn run_creates_then_reuses_worktree_and_session() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());

        app.run(repo.path(), "feature").unwrap();

        let id = Git::default().repository_id(repo.path()).unwrap();
        let target = paths.worktree_path(&id, "feature");
        let project = repo
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let expected = SessionMetadata {
            project_name: project,
            worktree_name: "feature".to_owned(),
            agent_name: "test".to_owned(),
        };
        assert!(target.is_dir());
        assert_eq!(sessions.state.borrow().created.len(), 1);
        assert_eq!(sessions.state.borrow().configured.len(), 1);
        assert_eq!(sessions.state.borrow().configured[0].1, expected);
        assert_eq!(sessions.state.borrow().attached.len(), 1);

        app.run(repo.path(), "feature").unwrap();
        assert_eq!(sessions.state.borrow().created.len(), 1);
        assert_eq!(sessions.state.borrow().configured.len(), 2);
        assert_eq!(sessions.state.borrow().configured[1].1, expected);
        assert_eq!(sessions.state.borrow().attached.len(), 2);
    }

    #[test]
    fn run_uses_the_atomically_returned_agent_pane_without_querying_panes() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = AtomicPaneSessions::default();
        let app = App::with_picker(paths.clone(), sessions.clone(), FirstAgentPicker);

        app.run(repo.path(), "feature").unwrap();

        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let state_path = paths.session_state_path(&repo_id, "feature");
        assert!(
            fs::read_to_string(state_path)
                .unwrap()
                .contains("pane=%99\n")
        );

        app.prompt(repo.path(), "feature", "message").unwrap();
        assert_eq!(
            sessions.inner.state.borrow().deliveries,
            vec![(
                session_name(&repo_id, "feature"),
                "message".to_owned(),
                Some("%99".to_owned())
            )]
        );
    }

    #[test]
    fn run_persists_agent_pane_and_prompt_uses_the_persisted_target() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = PaneSessions::default();
        sessions.state.borrow_mut().pane = Some("%42".to_owned());
        let app = App::with_picker(paths.clone(), sessions.clone(), FirstAgentPicker);

        app.run(repo.path(), "feature").unwrap();

        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let state_path = paths.session_state_path(&repo_id, "feature");
        assert!(
            fs::read_to_string(state_path)
                .unwrap()
                .contains("pane=%42\n")
        );

        app.prompt(repo.path(), "feature", "message").unwrap();
        assert_eq!(
            sessions.state.borrow().deliveries,
            vec![(
                session_name(&repo_id, "feature"),
                "message".to_owned(),
                Some("%42".to_owned())
            )]
        );
    }

    #[test]
    fn run_rejects_a_dead_agent_pane() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = PaneSessions::default();
        {
            let mut state = sessions.state.borrow_mut();
            state.pane = Some("%42".to_owned());
            state.pane_dead = true;
        }
        let app = App::with_picker(paths.clone(), sessions.clone(), FirstAgentPicker);

        let error = app.run(repo.path(), "feature").unwrap_err();

        assert!(error.to_string().contains("agent pane"));
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        assert!(!paths.session_state_path(&repo_id, "feature").exists());
        assert!(sessions.state.borrow().live.is_empty());
        assert!(sessions.state.borrow().killed.len() == 1);
    }

    #[test]
    fn prompt_rejects_a_dead_persisted_pane() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = PaneSessions::default();
        sessions.state.borrow_mut().pane = Some("%42".to_owned());
        let app = App::with_picker(paths, sessions.clone(), FirstAgentPicker);

        app.run(repo.path(), "feature").unwrap();
        sessions.state.borrow_mut().pane_dead = true;

        let error = app.prompt(repo.path(), "feature", "message").unwrap_err();

        assert!(error.to_string().contains("dead"));
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[test]
    fn run_cleans_up_state_and_session_when_agent_pane_query_fails() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = PaneSessions::default();
        sessions.state.borrow_mut().pane_error = Some("pane unavailable".to_owned());
        let app = App::with_picker(paths.clone(), sessions.clone(), FirstAgentPicker);

        let error = app.run(repo.path(), "feature").unwrap_err();

        assert!(error.to_string().contains("pane unavailable"));
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        assert!(!paths.session_state_path(&repo_id, "feature").exists());
        assert!(sessions.state.borrow().live.is_empty());
        assert_eq!(
            sessions.state.borrow().killed,
            vec![session_name(&repo_id, "feature")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_rejects_a_symlinked_worktree_parent_escape_before_creating_or_starting_session() {
        use std::os::unix::fs::symlink;

        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        fs::create_dir_all(&paths.worktrees).unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), paths.repository_worktrees(&repo_id)).unwrap();
        let sessions = FakeSessions::default();
        let app = test_app(paths, sessions.clone());

        let error = app.run(repo.path(), "feature").unwrap_err();

        assert!(error.to_string().contains("escapes the managed directory"));
        assert!(sessions.state.borrow().created.is_empty());
        assert!(!outside.path().join("feature").exists());
    }

    #[cfg(unix)]
    #[test]
    fn remove_rejects_a_symlinked_worktree_parent_escape_before_stopping_session() {
        use std::os::unix::fs::symlink;

        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let base = paths.repository_worktrees(&repo_id);
        let target = paths.worktree_path(&repo_id, worktree);
        let outside = tempfile::tempdir().unwrap();
        let moved = outside.path().join(worktree);
        fs::rename(&target, &moved).unwrap();
        fs::remove_dir(&base).unwrap();
        symlink(outside.path(), &base).unwrap();

        let error = app.remove(repo.path(), worktree, true).unwrap_err();

        assert!(error.to_string().contains("escapes the managed directory"));
        assert!(sessions.state.borrow().killed.is_empty());
        assert!(moved.is_dir());
    }

    #[test]
    fn run_supports_slashes_in_worktree_and_branch_names() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let name = "feature/login";

        app.run(repo.path(), name).unwrap();

        let id = Git::default().repository_id(repo.path()).unwrap();
        let target = paths.worktree_path(&id, name);
        assert!(target.is_dir());
        assert!(
            Command::new("git")
                .current_dir(repo.path())
                .args([
                    "show-ref",
                    "--verify",
                    "--quiet",
                    "refs/heads/feature/login"
                ])
                .status()
                .unwrap()
                .success()
        );

        app.run(repo.path(), name).unwrap();
        assert_eq!(sessions.state.borrow().created.len(), 1);
        assert_eq!(sessions.state.borrow().attached.len(), 2);

        let mut output = Vec::new();
        app.list(repo.path(), &mut output).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("feature/login\tfeature/login\ttest\t")
        );

        app.remove(repo.path(), name, true).unwrap();
        assert!(!target.exists());
        assert!(
            !Command::new("git")
                .current_dir(repo.path())
                .args([
                    "show-ref",
                    "--verify",
                    "--quiet",
                    "refs/heads/feature/login"
                ])
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn linked_worktrees_share_repository_identity() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());

        app.run(repo.path(), "first").unwrap();
        let first =
            paths.worktree_path(&Git::default().repository_id(repo.path()).unwrap(), "first");
        app.run(&first, "second").unwrap();

        let second = paths.worktree_path(
            &Git::default().repository_id(repo.path()).unwrap(),
            "second",
        );
        let project = repo
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(second.is_dir());
        assert_eq!(sessions.state.borrow().configured.len(), 2);
        assert_eq!(
            sessions.state.borrow().configured[0].1.project_name,
            project
        );
        assert_eq!(
            sessions.state.borrow().configured[1].1.project_name,
            sessions.state.borrow().configured[0].1.project_name
        );
        assert_eq!(
            sessions.state.borrow().configured[1].1.worktree_name,
            "second"
        );
    }

    #[test]
    fn remove_rejects_dirty_worktree_until_forced() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());

        app.run(repo.path(), "feature").unwrap();
        let target = paths.worktree_path(
            &Git::default().repository_id(repo.path()).unwrap(),
            "feature",
        );
        fs::write(target.join("uncommitted.txt"), "change\n").unwrap();

        assert!(app.remove(repo.path(), "feature", false).is_err());
        assert!(target.exists());
        assert_eq!(sessions.state.borrow().killed.len(), 0);

        app.remove(repo.path(), "feature", true).unwrap();
        assert!(!target.exists());
        assert_eq!(sessions.state.borrow().killed.len(), 1);
        let branch = Command::new("git")
            .current_dir(repo.path())
            .args(["show-ref", "--verify", "--quiet", "refs/heads/feature"])
            .status()
            .unwrap();
        assert!(!branch.success());

        app.run(repo.path(), "feature").unwrap();
        assert!(target.is_dir());
        assert_eq!(sessions.state.borrow().created.len(), 2);

        app.remove(&target, "feature", true).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn new_session_configuration_failure_rolls_back_session_and_state() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        sessions.state.borrow_mut().configure_error = Some("configuration failed".to_owned());
        let app = test_app(paths.clone(), sessions.clone());

        let error = app.run(repo.path(), "feature").unwrap_err();

        assert_eq!(error.to_string(), "configuration failed");
        let id = Git::default().repository_id(repo.path()).unwrap();
        let session = session_name(&id, "feature");
        let state_path = paths.session_state_path(&id, "feature");
        assert!(!sessions.state.borrow().live.contains(&session));
        assert_eq!(sessions.state.borrow().killed, vec![session]);
        assert!(!state_path.exists());
    }

    #[test]
    fn existing_session_configuration_failure_keeps_agent_alive() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths, sessions.clone());

        app.run(repo.path(), "feature").unwrap();
        sessions.state.borrow_mut().configure_error = Some("configuration failed".to_owned());

        let error = app.run(repo.path(), "feature").unwrap_err();

        assert_eq!(error.to_string(), "configuration failed");
        assert_eq!(sessions.state.borrow().killed.len(), 0);
        assert_eq!(sessions.state.borrow().attached.len(), 1);
        assert_eq!(sessions.state.borrow().live.len(), 1);
    }

    #[test]
    fn new_session_configuration_failure_keeps_state_when_cleanup_fails() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        sessions.state.borrow_mut().configure_error = Some("configuration failed".to_owned());
        sessions.state.borrow_mut().kill_error = Some("kill failed".to_owned());
        let app = test_app(paths.clone(), sessions.clone());

        let error = app.run(repo.path(), "feature").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed to clean up agent session")
        );
        let id = Git::default().repository_id(repo.path()).unwrap();
        let session = session_name(&id, "feature");
        let state_path = paths.session_state_path(&id, "feature");
        assert!(sessions.state.borrow().live.contains(&session));
        assert!(state_path.exists());
    }

    #[test]
    fn list_reports_active_agent() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths, sessions);
        app.run(repo.path(), "feature").unwrap();

        let mut output = Vec::new();
        app.list(repo.path(), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("feature\tfeature\ttest\t"));
    }

    #[test]
    fn human_list_preserves_header_and_row_bytes() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let app = test_app(paths.clone(), FakeSessions::default());
        app.run(repo.path(), "feature").unwrap();

        let target = paths.worktree_path(
            &Git::default().repository_id(repo.path()).unwrap(),
            "feature",
        );
        let target = fs::canonicalize(target).unwrap();
        let mut output = Vec::new();
        app.list(repo.path(), &mut output).unwrap();
        let expected = format!(
            "NAME\tBRANCH\tAGENT\tPATH\nfeature\tfeature\ttest\t{}\n",
            target.display()
        );
        assert_eq!(output, expected.as_bytes());
    }

    #[test]
    fn porcelain_list_reports_unknown_inactive_and_active_sessions() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());

        app.run(repo.path(), "active").unwrap();
        app.run(repo.path(), "inactive").unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        sessions
            .state
            .borrow_mut()
            .live
            .remove(&session_name(&repo_id, "inactive"));

        let unknown = paths.worktree_path(&repo_id, "unknown");
        fs::create_dir_all(unknown.parent().unwrap()).unwrap();
        app.git
            .add_worktree(repo.path(), "unknown", &unknown)
            .unwrap();

        let mut output = Vec::new();
        app.list_porcelain(repo.path(), false, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("name active\nbranch active\nagent test\nsession active\n"));
        assert!(output.contains("name inactive\nbranch inactive\nagent -\nsession inactive\n"));
        assert!(output.contains("name unknown\nbranch unknown\nagent -\nsession unknown\n"));
    }

    #[test]
    fn path_prints_only_the_absolute_managed_worktree_path() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions);
        app.run(repo.path(), "feature").unwrap();

        let target = paths.worktree_path(
            &Git::default().repository_id(repo.path()).unwrap(),
            "feature",
        );
        let expected = fs::canonicalize(target).unwrap();
        let mut output = Vec::new();
        app.path(repo.path(), "feature", false, &mut output)
            .unwrap();

        let mut expected_output = expected.as_os_str().to_string_lossy().into_owned();
        expected_output.push('\n');
        assert_eq!(output, expected_output.as_bytes());
    }

    #[test]
    fn path_rejects_missing_and_mismatched_worktrees() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions);
        let missing = app.path(repo.path(), "missing", false, &mut Vec::new());
        assert!(missing.is_err());

        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let target = paths.worktree_path(&repo_id, "feature");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        app.git.add_worktree(repo.path(), "other", &target).unwrap();

        let mismatched = app.path(repo.path(), "feature", false, &mut Vec::new());
        assert!(
            mismatched
                .unwrap_err()
                .to_string()
                .contains("expected branch")
        );
    }

    #[test]
    fn tmux_backend_configures_managed_session_affordances_when_tmux_is_available() {
        let Ok(available) = Command::new("tmux").arg("-V").output() else {
            return;
        };
        if !available.status.success() {
            return;
        }

        let session = format!(
            "david-test-{}-{}",
            std::process::id(),
            stable_hash("tmux-configure")
        );
        let directory = tempfile::tempdir().unwrap();
        let backend = TmuxBackend::default();
        let agent = Agent {
            command: "sleep".to_owned(),
            args: vec!["30".to_owned()],
        };
        let metadata = SessionMetadata {
            project_name: "source-%H-#{session_name}".to_owned(),
            worktree_name: "feature-#[fg=red]-%M".to_owned(),
            agent_name: "codex-#S-%d".to_owned(),
        };

        backend
            .create_session(&session, directory.path(), &agent)
            .unwrap();
        backend.configure_session(&session, &metadata).unwrap();
        assert!(backend.has_session(&session).unwrap());

        let show_option = |option: &str| {
            let mut command = backend.command();
            command.args(["show-option", "-v", "-t", &session, option]);
            let output = command.output().unwrap();
            assert!(output.status.success(), "show-option failed: {option}");
            text(&output.stdout).trim().to_owned()
        };
        let first_table = show_option("key-table");
        backend.configure_session(&session, &metadata).unwrap();
        let second_table = show_option("key-table");
        assert_ne!(first_table, second_table);
        assert_eq!(show_option("status"), "on");
        assert_eq!(show_option("@david-project"), metadata.project_name);
        assert_eq!(show_option("@david-worktree"), metadata.worktree_name);
        assert_eq!(show_option("@david-agent"), metadata.agent_name);
        assert!(show_option("status-left").contains("@david-project"));
        assert_eq!(show_option("status-right"), "detach: Ctrl-]");

        let table = second_table;
        assert!(session_key_tables(&session).contains(&table));
        let mut keys = backend.command();
        keys.args(["list-keys", "-T", &table, "-a"]);
        let output = keys.output().unwrap();
        assert!(output.status.success());
        let keys = text(&output.stdout);
        assert!(
            keys.lines()
                .any(|line| { line.contains("C-]") && line.contains("detach-client") })
        );
        assert!(keys.lines().any(|line| line.contains("MouseDown1Pane")));

        let mut prefix = backend.command();
        prefix.args(["list-keys", "-T", "prefix"]);
        let output = prefix.output().unwrap();
        assert!(output.status.success());
        let prefix = text(&output.stdout);
        assert!(
            prefix
                .lines()
                .any(|line| line.contains(" d ") && line.contains("detach-client"))
        );

        let mut display = backend.command();
        display.args([
            "display-message",
            "-p",
            "-t",
            &session,
            "#{T:status-left}|#{T:status-right}",
        ]);
        let output = display.output().unwrap();
        assert!(output.status.success());
        let rendered = text(&output.stdout);
        let expected = format!(
            "[DAVID] project: {} | worktree: {} | agent: {}|detach: Ctrl-]",
            metadata.project_name, metadata.worktree_name, metadata.agent_name
        );
        assert_eq!(rendered.trim(), expected);

        backend.kill_session(&session).unwrap();
        assert!(!backend.has_session(&session).unwrap());
        let mut keys = backend.command();
        keys.args(["list-keys", "-a"]);
        let output = keys.output().unwrap();
        assert!(output.status.success());
        let keys = text(&output.stdout);
        assert!(
            session_key_tables(&session)
                .iter()
                .all(|table| !keys.contains(table))
        );
    }

    #[test]
    fn list_reports_a_dead_persisted_pane_as_inactive() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = PaneSessions::default();
        sessions.state.borrow_mut().pane = Some("%42".to_owned());
        let app = App::with_picker(paths, sessions.clone(), FirstAgentPicker);

        app.run(repo.path(), "feature").unwrap();
        sessions.state.borrow_mut().pane_dead = true;

        let mut output = Vec::new();
        app.list(repo.path(), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("feature\tfeature\t-\t"));
        assert!(!output.contains("feature\tfeature\ttest\t"));

        let mut porcelain = Vec::new();
        app.list_porcelain(repo.path(), false, &mut porcelain)
            .unwrap();
        let porcelain = String::from_utf8(porcelain).unwrap();
        assert!(porcelain.contains("agent -\nsession active\n"));
    }

    #[test]
    fn porcelain_list_keeps_a_live_session_active_when_pane_query_fails() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = PaneSessions::default();
        let app = App::with_picker(paths.clone(), sessions.clone(), FirstAgentPicker);

        app.run(repo.path(), "feature").unwrap();
        sessions.state.borrow_mut().pane_error = Some("pane query failed".to_owned());

        let mut output = Vec::new();
        app.list_porcelain(repo.path(), false, &mut output).unwrap();
        let target = paths.worktree_path(
            &Git::default().repository_id(repo.path()).unwrap(),
            "feature",
        );
        let target = fs::canonicalize(target).unwrap();
        let expected = format!(
            "name feature\nbranch feature\nagent -\nsession active\npath {}\n",
            target.display()
        );
        assert_eq!(output, expected.as_bytes());
    }

    #[test]
    fn porcelain_list_does_not_mark_an_unmanaged_live_session_active() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions);

        app.run(repo.path(), "feature").unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        fs::remove_file(paths.session_state_path(&repo_id, "feature")).unwrap();

        let mut output = Vec::new();
        app.list_porcelain(repo.path(), false, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("agent -\nsession unknown\n"));
        assert!(!output.contains("session active\n"));
    }

    #[test]
    fn list_preserves_active_status_for_a_live_legacy_session() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions);

        app.run(repo.path(), "feature").unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let state_path = paths.session_state_path(&repo_id, "feature");
        let legacy = fs::read_to_string(&state_path)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("pane="))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(state_path, legacy).unwrap();

        let mut output = Vec::new();
        app.list(repo.path(), &mut output).unwrap();

        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("feature\tfeature\ttest\t")
        );
    }

    #[test]
    fn prompt_forwards_exact_message_without_loading_config_or_attaching() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let attached = sessions.state.borrow().attached.len();
        let created = sessions.state.borrow().created.len();
        fs::remove_file(paths.config_path()).unwrap();

        let message = "-literal 'quotes' $() 😀\tline one\nline two";
        app.prompt(repo.path(), worktree, message).unwrap();

        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        assert_eq!(
            sessions.state.borrow().deliveries,
            vec![(session_name(&repo_id, worktree), message.to_owned())]
        );
        assert_eq!(sessions.state.borrow().attached.len(), attached);
        assert_eq!(sessions.state.borrow().created.len(), created);
    }

    #[test]
    fn prompt_rejects_unknown_worktree_and_does_not_deliver() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = DavidPaths::from_home(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths, sessions.clone());

        let error = app.prompt(repo.path(), "unknown", "message").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("managed worktree does not exist")
        );
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[test]
    fn prompt_rejects_dead_session() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths, sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        sessions
            .state
            .borrow_mut()
            .live
            .remove(&session_name(&repo_id, worktree));

        let error = app.prompt(repo.path(), worktree, "message").unwrap_err();

        assert!(error.to_string().contains("not running"));
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[test]
    fn prompt_rejects_a_deleted_worktree_checkout() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let target = paths.worktree_path(&repo_id, worktree);
        fs::remove_dir_all(&target).unwrap();

        let error = app.prompt(repo.path(), worktree, "message").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("managed worktree does not exist")
        );
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn prompt_rejects_a_worktree_symlink_escape() {
        use std::os::unix::fs::symlink;

        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let target = paths.worktree_path(&repo_id, worktree);
        let outside = tempfile::tempdir().unwrap();
        let moved = outside.path().join("checkout");
        fs::rename(&target, &moved).unwrap();
        symlink(&moved, &target).unwrap();

        let error = app.prompt(repo.path(), worktree, "message").unwrap_err();

        assert!(error.to_string().contains("escapes the managed directory"));
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn list_omits_a_registered_worktree_that_resolves_outside_the_managed_directory() {
        use std::os::unix::fs::symlink;

        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions);
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let target = paths.worktree_path(&repo_id, worktree);
        let outside = tempfile::tempdir().unwrap();
        let moved = outside.path().join(worktree);
        fs::rename(&target, &moved).unwrap();
        symlink(&moved, &target).unwrap();

        let mut output = Vec::new();
        app.list(repo.path(), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(!output.contains("feature\tfeature\t"));
        assert!(output.contains("No managed worktrees."));
    }

    #[test]
    fn prompt_rejects_unmanaged_live_session() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        fs::remove_file(paths.session_state_path(&repo_id, worktree)).unwrap();

        let error = app.prompt(repo.path(), worktree, "message").unwrap_err();

        assert!(error.to_string().contains("not managed by david"));
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[test]
    fn prompt_rejects_mismatched_session_metadata() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths.clone(), sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let repo_id = Git::default().repository_id(repo.path()).unwrap();
        let state_path = paths.session_state_path(&repo_id, worktree);
        let state = fs::read_to_string(&state_path).unwrap();
        fs::write(
            &state_path,
            state.replace("worktree_name=feature", "worktree_name=other"),
        )
        .unwrap();

        let error = app.prompt(repo.path(), worktree, "message").unwrap_err();

        assert!(error.to_string().contains("metadata does not match"));
        assert!(sessions.state.borrow().deliveries.is_empty());
    }

    #[test]
    fn prompt_reports_backend_delivery_failure_without_attach_or_create() {
        let repo = init_repo();
        let home = tempfile::tempdir().unwrap();
        let paths = configured_paths(home.path());
        let sessions = FakeSessions::default();
        let app = test_app(paths, sessions.clone());
        let worktree = "feature";

        app.run(repo.path(), worktree).unwrap();
        let attached = sessions.state.borrow().attached.len();
        let created = sessions.state.borrow().created.len();
        sessions.state.borrow_mut().prompt_error = Some("transport unavailable".to_owned());

        let error = app.prompt(repo.path(), worktree, "message").unwrap_err();

        assert!(error.to_string().contains("transport unavailable"));
        assert!(sessions.state.borrow().deliveries.is_empty());
        assert_eq!(sessions.state.borrow().attached.len(), attached);
        assert_eq!(sessions.state.borrow().created.len(), created);
    }

    #[cfg(unix)]
    #[test]
    fn tmux_prompt_uses_exact_bytes_and_targeted_buffer_sequence() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fake-tmux");
        fs::write(
            &program,
            "#!/bin/sh\ncase \" $* \" in\n  *\" list-panes \"*) printf '%s\\n' '%42|david-managed|0' ;;\n  *\" display-message \"*) printf '%s\\n' 'david-managed|0|0' ;;\n  *) printf '%s\\n' \"$@\" > \"$0.args\"; cat > \"$0.stdin\" ;;\nesac\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        let backend = TmuxBackend::new(program.as_os_str().to_owned());
        let message = "-literal 'quotes' $() 😀\tline one\nline two";
        backend.deliver_prompt("david-managed", message).unwrap();

        let args_path = program.with_extension("args");
        let args: Vec<String> = fs::read_to_string(args_path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let load = args.iter().position(|arg| arg == "load-buffer").unwrap();
        let buffer = args[load + 2].clone();
        let pane = "=david-managed:0.%42";
        assert!(buffer.is_ascii());
        assert_eq!(
            &args[load..],
            &[
                "load-buffer",
                "-b",
                buffer.as_str(),
                "-",
                ";",
                "paste-buffer",
                "-dprS",
                "-b",
                buffer.as_str(),
                "-t",
                pane,
                ";",
                "send-keys",
                "-t",
                pane,
                "Enter",
            ]
        );
        assert!(!args.iter().any(|arg| arg == message));
        assert_eq!(
            fs::read(program.with_extension("stdin")).unwrap(),
            message.as_bytes()
        );

        backend.deliver_prompt("david-managed", "").unwrap();
        let empty_args: Vec<String> = fs::read_to_string(program.with_extension("args"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert!(!empty_args.iter().any(|arg| arg == "load-buffer"));
        assert!(!empty_args.iter().any(|arg| arg == "paste-buffer"));
        assert_eq!(
            empty_args
                .iter()
                .filter(|arg| arg.as_str() == "send-keys")
                .count(),
            1
        );
        assert!(
            fs::read(program.with_extension("stdin"))
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn tmux_create_session_captures_the_initial_pane_from_new_session_output() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fake-tmux");
        fs::write(
            &program,
            "#!/bin/sh\ncount_file=\"$0.count\"\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); else count=0; fi\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > \"$count_file\"\nprintf '%s\\n' \"$@\" > \"$0.args.$count\"\ncase \" $* \" in\n  *\" new-session \"*) printf '%s\\n' '%42' ;;\n  *\" list-windows \"*) printf '%s\\n' '7' ;;\nesac\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        let backend = TmuxBackend::new(program.as_os_str().to_owned());
        let agent = Agent {
            command: "echo".to_owned(),
            args: vec!["ready".to_owned()],
        };
        let pane = backend
            .create_session_with_pane("david-managed", directory.path(), &agent)
            .unwrap();

        assert_eq!(pane.as_deref(), Some("%42"));
        let new_args: Vec<String> = fs::read_to_string(program.with_extension("args.1"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let start = new_args
            .iter()
            .position(|arg| arg == "new-session")
            .unwrap();
        assert_eq!(
            &new_args[start..start + 7],
            [
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                "david-managed",
            ]
        );
        assert_eq!(
            new_args[start + 7..],
            [
                "-c".to_owned(),
                directory.path().to_string_lossy().into_owned(),
                "--".to_owned(),
                "echo".to_owned(),
                "ready".to_owned(),
            ]
        );

        let status_args: Vec<String> = fs::read_to_string(program.with_extension("args.3"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let status = status_args
            .iter()
            .position(|arg| arg == "set-option")
            .unwrap();
        assert_eq!(
            &status_args[status..],
            ["set-option", "-t", "=david-managed:7", "status", "off"]
        );
        let all_args = (1..=3)
            .map(|number| fs::read_to_string(program.with_extension(format!("args.{number}"))))
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap()
            .join("\n");
        assert!(!all_args.contains("list-panes"));
    }

    #[cfg(unix)]
    #[test]
    fn tmux_session_window_target_uses_the_first_window_index_and_exact_session() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fake-tmux");
        fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nprintf '2\\n3\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        let backend = TmuxBackend::new(program.as_os_str().to_owned());
        assert_eq!(
            backend.session_window_target("david-managed").unwrap(),
            "=david-managed:2"
        );
        let args: Vec<String> = fs::read_to_string(program.with_extension("args"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let target = args.iter().position(|arg| arg == "-t").unwrap();
        assert_eq!(args[target + 1], "=david-managed");
    }

    #[cfg(unix)]
    #[test]
    fn tmux_agent_pane_rejects_a_dead_pane() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fake-tmux");
        fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' '%42|david-managed|1'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        let backend = TmuxBackend::new(program.as_os_str().to_owned());
        let error = backend.agent_pane("david-managed").unwrap_err();

        assert!(error.to_string().contains("dead"));
    }

    #[cfg(unix)]
    #[test]
    fn tmux_persisted_pane_liveness_rejects_dead_pane_targeting() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fake-tmux");
        fs::write(
            &program,
            "#!/bin/sh\ncase \" $* \" in\n  *window_index*) printf '%s\\n' 'david-managed|2|1' ;;\n  *) printf '%s\\n' 'david-managed|1' ;;\nesac\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        let backend = TmuxBackend::new(program.as_os_str().to_owned());
        assert!(!backend.pane_is_alive("david-managed", "%42").unwrap());
        let error = backend
            .deliver_prompt_to("david-managed", "message", Some("%42"))
            .unwrap_err();

        assert!(error.to_string().contains("dead"));
    }

    #[test]
    fn missing_tmux_reports_a_prerequisite_error() {
        let directory = tempfile::tempdir().unwrap();
        let backend = TmuxBackend::new(directory.path().join("missing-tmux"));

        let error = backend.ensure_available().unwrap_err();

        assert!(error.to_string().contains("tmux is required"));
    }

    #[cfg(unix)]
    #[test]
    fn tmux_prompt_reports_transport_failure() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("failing-tmux");
        fs::write(
            &program,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' 'delivery failed' >&2\nexit 7\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        let backend = TmuxBackend::new(program.as_os_str().to_owned());
        let error = backend
            .deliver_prompt_at("=david-managed:0.0", "message")
            .unwrap_err();

        assert!(error.to_string().contains("delivery failed"));
    }

    #[cfg(unix)]
    #[test]
    fn tmux_backend_delivers_prompt_to_the_managed_agent_pane() {
        use std::os::unix::fs::PermissionsExt;

        let Ok(available) = Command::new("tmux").arg("-V").output() else {
            return;
        };
        if !available.status.success() {
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let reader = directory.path().join("reader.sh");
        fs::write(
            &reader,
            "#!/bin/sh\nstty raw -echo\ndd bs=1 count=\"$1\" of=\"$2\" 2>/dev/null\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&reader).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&reader, permissions).unwrap();

        let output = directory.path().join("received");
        let message = "literal Enter C-c Space; $() 😀\nsecond line";
        let expected_len = message.len() + 1;
        let session = format!(
            "david-test-{}-{}",
            std::process::id(),
            stable_hash("prompt-delivery")
        );
        let backend = TmuxBackend::default();
        let agent = Agent {
            command: reader.to_string_lossy().into_owned(),
            args: vec![
                expected_len.to_string(),
                output.to_string_lossy().into_owned(),
            ],
        };

        backend
            .create_session(&session, directory.path(), &agent)
            .unwrap();
        let pane = backend.agent_pane(&session).unwrap().unwrap();
        assert!(pane.starts_with('%'));
        let delivery = backend.deliver_prompt_to(&session, message, Some(&pane));

        let mut received = None;
        for _ in 0..100 {
            if output.is_file() && fs::metadata(&output).unwrap().len() == expected_len as u64 {
                received = Some(fs::read(&output).unwrap());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        backend.kill_session(&session).unwrap();
        delivery.unwrap();

        let mut expected = message.as_bytes().to_vec();
        expected.push(b'\n');
        assert_eq!(received, Some(expected));
    }

    #[test]
    fn tmux_backend_manages_a_session_when_tmux_is_available() {
        let Ok(available) = Command::new("tmux").arg("-V").output() else {
            return;
        };
        if !available.status.success() {
            return;
        }

        let session = format!(
            "david-test-{}-{}",
            std::process::id(),
            stable_hash("tmux-manages")
        );
        let directory = tempfile::tempdir().unwrap();
        let backend = TmuxBackend::default();
        let agent = Agent {
            command: "sleep".to_owned(),
            args: vec!["30".to_owned()],
        };

        backend
            .create_session(&session, directory.path(), &agent)
            .unwrap();
        assert!(backend.has_session(&session).unwrap());
        backend.kill_session(&session).unwrap();
        assert!(!backend.has_session(&session).unwrap());
    }
}
