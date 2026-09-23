//! Shared program launch definitions, independent of their owner.

use serde::{Deserialize, Serialize};

/// A persisted Windows launch definition used by bottles and standalone programs.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramSpec {
    name: String,
    executable: String,
    /// Command-line fragments joined with spaces before launch.
    ///
    /// Callers must include any Windows quoting needed for spaces or special
    /// characters inside an argument. An empty entry adds only a separator and
    /// does not produce an empty Windows argument.
    #[serde(default)]
    args: Vec<String>,
    /// Windows working directory, or WineBridge's inherited directory when absent.
    #[serde(default)]
    working_directory: Option<String>,
    /// Passed to WineBridge's `CREATE_NEW_CONSOLE` launch option.
    #[serde(default)]
    new_console: bool,
}

impl ProgramSpec {
    /// Creates a launch definition with default launch options.
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        }
    }

    /// Replaces the Windows command-line fragments passed at launch.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        self
    }

    /// Sets the Windows working directory used at launch.
    pub fn with_working_directory(mut self, working_directory: impl Into<String>) -> Self {
        self.working_directory = Some(working_directory.into());
        self
    }

    /// Controls WineBridge's `CREATE_NEW_CONSOLE` launch option.
    pub fn with_new_console(mut self, new_console: bool) -> Self {
        self.new_console = new_console;
        self
    }

    /// Changes the display name.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Returns the display name preserved at registration.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Windows executable path passed to WineBridge.
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Returns the command-line fragments passed at launch.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the configured Windows working directory, when present.
    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// Reports whether launch requests create a new console.
    pub fn new_console(&self) -> bool {
        self.new_console
    }
}
