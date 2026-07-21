use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
};
use tokio::process::{Child, Command as TokioCommand};

pub(crate) trait Wrapper: Into<Command> + Sized {
    fn wrap<W: Wrapper>(self, inner: W) -> Wrapped<Self, W> {
        Wrapped { outer: self, inner }
    }
}

#[derive(Debug)]
pub(crate) struct Wrapped<O: Wrapper, I: Wrapper> {
    outer: O,
    inner: I,
}

impl<O: Wrapper, I: Wrapper> Wrapper for Wrapped<O, I> {}
impl<O: Wrapper, I: Wrapper> From<Wrapped<O, I>> for Command {
    fn from(wrapped: Wrapped<O, I>) -> Self {
        wrapped.outer.into().append(wrapped.inner.into())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Command {
    executable: OsString,
    args: Vec<OsString>,
    envs: HashMap<OsString, OsString>,
}

impl Wrapper for Command {}

impl Command {
    pub(crate) fn new(executable: impl AsRef<OsStr>) -> Self {
        Self {
            executable: executable.as_ref().to_os_string(),
            args: Vec::new(),
            envs: HashMap::new(),
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

    fn append(mut self, inner: Self) -> Self {
        self.args.push(inner.executable);
        self.args.extend(inner.args);
        self.envs.extend(inner.envs);
        self
    }

    pub(crate) fn spawn(self) -> std::io::Result<Child> {
        TokioCommand::new(self.executable)
            .args(self.args)
            .envs(self.envs)
            .spawn()
    }
}
