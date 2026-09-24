//! Type-safe composition of nested process wrappers.

use super::{Command, Spawnable};

/// Composes an outer host command around an inner command without a shell.
pub(crate) trait Wrapper: Into<Command> + Sized {
    /// Places `self` outside `inner` without spawning either command.
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

impl<O: Wrapper, I: Spawnable> Spawnable for Wrapped<O, I> {}

impl Wrapper for Command {}
