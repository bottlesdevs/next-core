//! Portable descriptions of Windows processes launched through `WineBridge`.

use serde::{Deserialize, Serialize};

/// Describes one Windows executable and its launch options.
///
/// `WineBridge` receives paths and arguments as Windows strings without checking
/// path existence or parsing the command line. At launch it rejects an empty
/// executable and NUL bytes in the executable, arguments, or working directory.
/// Argument fragments are joined with a single space, so callers must supply
/// Windows quoting within each fragment. Deserialization rejects unknown fields;
/// omitted arguments, working directory, and new-console flag use their constructor
/// defaults.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramSpec {
    name: String,
    executable: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    working_directory: Option<String>,
    #[serde(default)]
    new_console: bool,
}

impl ProgramSpec {
    /// Creates a launch definition with no arguments, working directory, or new console.
    ///
    /// The display `name` is not required to be unique. The `executable` is
    /// preserved verbatim for `WineBridge`.
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        }
    }

    /// Replaces the command-line fragments passed at launch.
    ///
    /// `WineBridge` joins the fragments with one space and does not add quoting.
    /// Callers are responsible for preserving each intended Windows argument,
    /// including quoting whitespace and escaping embedded quote characters. An
    /// empty fragment contributes only a separator; it cannot encode an empty
    /// Windows argument.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        self
    }

    /// Sets the Windows working directory used at launch.
    ///
    /// Without one, the process inherits `WineBridge`'s working directory.
    pub fn with_working_directory(mut self, working_directory: impl Into<String>) -> Self {
        self.working_directory = Some(working_directory.into());
        self
    }

    /// Controls `WineBridge`'s `CREATE_NEW_CONSOLE` launch option.
    pub fn with_new_console(mut self, new_console: bool) -> Self {
        self.new_console = new_console;
        self
    }

    /// Replaces the display name without changing launch behavior.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Returns the display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Windows executable path passed to `WineBridge`.
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Returns the command-line fragments in their configured order.
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
