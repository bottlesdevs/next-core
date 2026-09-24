//! Command construction, wrapper composition, and process spawning.
//!
//! A [`Command`] owns an executable, its arguments, and environment overrides.
//! Wrappers prepend another command without invoking a shell, so argument and
//! environment boundaries remain explicit.

use crate::EnvVars;
use async_process::{Child, Command as AsyncCommand};
use std::ffi::{OsStr, OsString};

mod wrapper;
pub(crate) mod wrappers;
pub(crate) use wrapper::Wrapper;

/// Marks a command representation that can be lowered and spawned directly.
pub(crate) trait Spawnable: Into<Command> + Sized {
    /// Converts this value into a command and starts the child process.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the process cannot be spawned.
    fn spawn(self) -> std::io::Result<Child> {
        let command = self.into();
        AsyncCommand::new(command.executable)
            .args(command.args)
            .envs(command.env_vars)
            .spawn()
    }
}

/// An owned process invocation before it is spawned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Command {
    executable: OsString,
    args: Vec<OsString>,
    env_vars: EnvVars<OsString>,
}

impl Command {
    pub(crate) fn new(executable: impl AsRef<OsStr>) -> Self {
        Self {
            executable: executable.as_ref().to_os_string(),
            args: Vec::new(),
            env_vars: EnvVars::default(),
        }
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub(crate) fn args<A: AsRef<OsStr>>(mut self, args: impl IntoIterator<Item = A>) -> Self {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    /// Sets an environment override, replacing an existing value for `key`.
    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env_vars
            .insert(key.as_ref().to_os_string(), value.as_ref().to_os_string());
        self
    }

    /// Extends the environment overrides from `(key, value)` pairs.
    pub(crate) fn envs<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        mut self,
        envs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.env_vars.extend(
            envs.into_iter()
                .map(|(key, value)| (key.as_ref().to_os_string(), value.as_ref().to_os_string())),
        );
        self
    }

    /// Appends `inner` as the command executed by this wrapper.
    ///
    /// The inner executable becomes the next argument, followed by its
    /// arguments. Inner environment values take precedence on duplicate keys.
    fn append(mut self, inner: Command) -> Command {
        self.args.push(inner.executable);
        self.args.extend(inner.args);
        self.env_vars.extend(inner.env_vars);
        self
    }
}
