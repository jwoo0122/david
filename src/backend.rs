use david::{Agent, DavidPaths, Result, SessionBackend, SessionMetadata, TmuxBackend, ToolError};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Default)]
struct DirectSession {
    cwd: PathBuf,
    metadata: Option<SessionMetadata>,
    reserved: bool,
}

#[derive(Debug, Default)]
pub(crate) struct DirectBackend {
    sessions: Mutex<BTreeMap<String, DirectSession>>,
    legacy: TmuxBackend,
}

impl DirectBackend {
    fn is_direct(&self, name: &str) -> bool {
        self.sessions
            .lock()
            .expect("direct session mutex poisoned")
            .contains_key(name)
    }

    fn optional_tmux<T>(result: Result<T>, fallback: T) -> Result<T> {
        match result {
            Err(ToolError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(fallback),
            result => result,
        }
    }
}

impl SessionBackend for DirectBackend {
    fn ensure_available(&self) -> Result<()> {
        Ok(())
    }

    fn has_session(&self, name: &str) -> Result<bool> {
        if self.is_direct(name) {
            Ok(true)
        } else {
            Self::optional_tmux(self.legacy.has_session(name), false)
        }
    }

    fn create_session(&self, name: &str, cwd: &Path, _agent: &Agent) -> Result<()> {
        self.sessions
            .lock()
            .expect("direct session mutex poisoned")
            .insert(
                name.to_owned(),
                DirectSession {
                    cwd: cwd.to_path_buf(),
                    ..DirectSession::default()
                },
            );
        Ok(())
    }

    fn create_session_with_pane(
        &self,
        name: &str,
        cwd: &Path,
        agent: &Agent,
    ) -> Result<Option<String>> {
        self.create_session(name, cwd, agent)?;
        Ok(Some("%0".to_owned()))
    }

    fn pane_is_alive(&self, name: &str, pane: &str) -> Result<bool> {
        if self.is_direct(name) {
            Ok(pane == "%0")
        } else {
            self.legacy.pane_is_alive(name, pane)
        }
    }

    fn configure_session_affordances(&self, name: &str, metadata: &SessionMetadata) -> Result<()> {
        if self.is_direct(name) {
            Ok(())
        } else {
            self.legacy.configure_session_affordances(name, metadata)
        }
    }

    fn configure_session(&self, name: &str, metadata: &SessionMetadata) -> Result<()> {
        if self.is_direct(name) {
            let mut sessions = self.sessions.lock().expect("direct session mutex poisoned");
            let session = sessions.get_mut(name).ok_or_else(|| {
                ToolError::Message(format!("direct session {name} no longer exists"))
            })?;
            session.metadata = Some(metadata.clone());
            Ok(())
        } else {
            self.legacy.configure_session(name, metadata)
        }
    }

    fn validate_session_metadata(&self, name: &str, metadata: &SessionMetadata) -> Result<()> {
        if self.is_direct(name) {
            let sessions = self.sessions.lock().expect("direct session mutex poisoned");
            let session = sessions.get(name).ok_or_else(|| {
                ToolError::Message(format!("direct session {name} no longer exists"))
            })?;
            if session.metadata.as_ref() == Some(metadata) {
                Ok(())
            } else {
                Err(ToolError::Message(format!(
                    "direct session {name} metadata does not match"
                )))
            }
        } else {
            self.legacy.validate_session_metadata(name, metadata)
        }
    }

    fn agent_started(&self, name: &str) -> Result<bool> {
        if self.is_direct(name) {
            Ok(false)
        } else {
            self.legacy.agent_started(name)
        }
    }

    fn reserve_agent_start(&self, name: &str) -> Result<bool> {
        if self.is_direct(name) {
            let mut sessions = self.sessions.lock().expect("direct session mutex poisoned");
            let session = sessions.get_mut(name).ok_or_else(|| {
                ToolError::Message(format!("direct session {name} no longer exists"))
            })?;
            if session.reserved {
                Ok(false)
            } else {
                session.reserved = true;
                Ok(true)
            }
        } else {
            self.legacy.reserve_agent_start(name)
        }
    }

    fn attach_with_agent(&self, name: &str, pane: &str, agent: &Agent) -> Result<()> {
        if !self.is_direct(name) {
            return self.legacy.attach_with_agent(name, pane, agent);
        }
        if pane != "%0" {
            return Err(ToolError::Message(format!(
                "direct session {name} has an invalid pane target"
            )));
        }

        let cwd = {
            let mut sessions = self.sessions.lock().expect("direct session mutex poisoned");
            sessions
                .remove(name)
                .ok_or_else(|| {
                    ToolError::Message(format!("direct session {name} no longer exists"))
                })?
                .cwd
        };
        remove_session_metadata(name)?;

        let mut command = Command::new(&agent.command);
        command.current_dir(cwd).args(&agent.args);

        #[cfg(unix)]
        {
            let error = command.exec();
            Err(ToolError::Io(error))
        }
        #[cfg(not(unix))]
        {
            let status = command.status()?;
            if status.success() {
                Ok(())
            } else {
                Err(ToolError::Command {
                    program: agent.command.clone(),
                    detail: status.to_string(),
                })
            }
        }
    }

    fn attach(&self, name: &str) -> Result<()> {
        if self.is_direct(name) {
            Err(ToolError::Message(
                "direct sessions cannot be reattached; rerun the worktree instead".to_owned(),
            ))
        } else {
            self.legacy.attach(name)
        }
    }

    fn clear_session_affordances(&self, name: &str) -> Result<()> {
        if self.is_direct(name) {
            Ok(())
        } else {
            Self::optional_tmux(self.legacy.clear_session_affordances(name), ())
        }
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        if self
            .sessions
            .lock()
            .expect("direct session mutex poisoned")
            .remove(name)
            .is_some()
        {
            remove_session_metadata(name)
        } else {
            Self::optional_tmux(self.legacy.kill_session(name), ())
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct AgentSelectionConfig {
    #[serde(default)]
    default_agent: Option<String>,
    #[serde(default)]
    agents: BTreeMap<String, toml::Value>,
}

pub(crate) fn direct_agent_is_resolvable(
    paths: &DavidPaths,
    explicit: Option<&str>,
) -> Result<bool> {
    if explicit.is_some() || env::var("DAVID_AGENT").is_ok_and(|agent| !agent.is_empty()) {
        return Ok(true);
    }
    let content = match fs::read_to_string(paths.config_path()) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(ToolError::Io(error)),
    };
    let config: AgentSelectionConfig = match toml::from_str(&content) {
        Ok(config) => config,
        Err(_) => return Ok(true),
    };
    Ok(config.default_agent.is_some() || config.agents.len() == 1)
}

fn session_state_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let xdg_state_root = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());

    if let Some(state_root) = &xdg_state_root {
        directories.push(state_root.join("david/sessions"));
    }
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        if xdg_state_root.is_none() {
            directories.push(home.join(".local/state/david/sessions"));
        }
        directories.push(home.join(".david/sessions"));
    }

    directories.sort();
    directories.dedup();
    directories
}

fn remove_session_metadata(session: &str) -> Result<()> {
    let marker = format!("session={session}");
    for directory in session_state_directories() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(ToolError::Io(error)),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("state") {
                continue;
            }
            let content = fs::read_to_string(&path)?;
            if content.lines().any(|line| line == marker) {
                fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}
