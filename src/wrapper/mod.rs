pub(crate) mod gamescope;
pub(crate) mod mangohud;

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use tokio::process::{Child, Command as TokioCommand};

use crate::{runner::RunnerCommand, utils::environment::Environment};

use self::{
    gamescope::{Gamescope, GamescopeConfig},
    mangohud::{MangoHud, MangoHudConfig},
};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Wrappers {
    #[serde(default)]
    pub gamescope: GamescopeConfig,
    #[serde(default)]
    pub mangohud: MangoHudConfig,
}

impl Wrappers {
    pub(crate) fn apply(&self, command: RunnerCommand) -> RunnerCommand {
        match (self.gamescope.enabled, self.mangohud.enabled) {
            (false, false) => command,
            (false, true) => command.wrapped_by(MangoHud::from(self.mangohud.clone())),
            (true, false) => command.wrapped_by(Gamescope::from(self.gamescope.clone())),
            (true, true) => {
                command.wrapped_by(Gamescope::from(self.gamescope.clone()).with_mangoapp())
            }
        }
    }
}

pub(crate) trait Wrapper: Into<Command> + Sized {
    fn wrap<I: Into<Command>>(self, inner: I) -> Wrapped<Self, I> {
        Wrapped { outer: self, inner }
    }
}

#[derive(Debug)]
pub(crate) struct Wrapped<O: Wrapper, I: Into<Command>> {
    outer: O,
    inner: I,
}

impl<O: Wrapper, I: Into<Command>> Wrapper for Wrapped<O, I> {}
impl<O: Wrapper, I: Into<Command>> From<Wrapped<O, I>> for Command {
    fn from(wrapped: Wrapped<O, I>) -> Self {
        wrapped.outer.into().append(wrapped.inner.into())
    }
}

pub(crate) trait Spawnable: Into<Command> + Sized {
    fn spawn(self) -> std::io::Result<Child> {
        let command = self.into();
        TokioCommand::new(command.executable)
            .args(command.args)
            .envs(command.envs)
            .spawn()
    }
}

impl<O: Wrapper, I: Spawnable> Spawnable for Wrapped<O, I> {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Command {
    executable: OsString,
    args: Vec<OsString>,
    envs: Environment<OsString>,
}

impl Wrapper for Command {}

impl Command {
    pub(crate) fn new(executable: impl AsRef<OsStr>) -> Self {
        Self {
            executable: executable.as_ref().to_os_string(),
            args: Vec::new(),
            envs: Environment::default(),
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

    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .insert(key.as_ref().to_os_string(), value.as_ref().to_os_string());
        self
    }

    pub(crate) fn envs<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        mut self,
        envs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.envs.extend(
            envs.into_iter()
                .map(|(key, value)| (key.as_ref().to_os_string(), value.as_ref().to_os_string())),
        );
        self
    }

    fn append(mut self, inner: Command) -> Command {
        self.args.push(inner.executable);
        self.args.extend(inner.args);
        self.envs.extend(inner.envs);
        self
    }
}
