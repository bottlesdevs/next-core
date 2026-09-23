//! Portable descriptions of Windows processes launched through WineBridge.
//!
//! A [`ProgramSpec`] contains only launch metadata. Bottles can store several
//! specifications, while a standalone program uses one as its persisted data.

use serde::{Deserialize, Serialize};

/// Describes one Windows executable and its launch options.
///
/// Paths and arguments are passed to WineBridge as Windows strings; this type
/// does not validate that they exist in a particular prefix.
///
/// # Examples
///
/// ```
/// use bottles_core::ProgramSpec;
///
/// let program = ProgramSpec::new("Editor", r"C:\Tools\editor.exe")
///     .with_args(["document.txt"])
///     .with_working_directory(r"C:\Users\Public");
///
/// assert_eq!(program.executable(), r"C:\Tools\editor.exe");
/// assert_eq!(program.working_directory(), Some(r"C:\Users\Public"));
/// ```
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
    /// Creates a launch definition with no arguments or working directory.
    ///
    /// The display `name` is not required to be unique. The `executable` is
    /// preserved verbatim for WineBridge.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Calculator", r"C:\windows\calc.exe");
    /// assert_eq!(program.name(), "Calculator");
    /// assert!(program.args().is_empty());
    /// ```
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
    /// WineBridge joins the fragments with spaces. Callers are responsible for
    /// any quoting required within a fragment.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Tool", "tool.exe")
    ///     .with_args(["--mode", "safe"]);
    /// assert_eq!(program.args(), &["--mode", "safe"]);
    /// ```
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
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Tool", "tool.exe")
    ///     .with_working_directory(r"C:\Tools");
    /// assert_eq!(program.working_directory(), Some(r"C:\Tools"));
    /// ```
    pub fn with_working_directory(mut self, working_directory: impl Into<String>) -> Self {
        self.working_directory = Some(working_directory.into());
        self
    }

    /// Controls WineBridge's `CREATE_NEW_CONSOLE` launch option.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Console", "console.exe")
    ///     .with_new_console(true);
    /// assert!(program.new_console());
    /// ```
    pub fn with_new_console(mut self, new_console: bool) -> Self {
        self.new_console = new_console;
        self
    }

    /// Replaces the display name without changing launch behavior.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let mut program = ProgramSpec::new("Old", "tool.exe");
    /// program.rename("New");
    /// assert_eq!(program.name(), "New");
    /// ```
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Returns the display name.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Paint", "paint.exe");
    /// assert_eq!(program.name(), "Paint");
    /// ```
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Windows executable path passed to WineBridge.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Paint", r"C:\paint.exe");
    /// assert_eq!(program.executable(), r"C:\paint.exe");
    /// ```
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Returns the command-line fragments in their configured order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Tool", "tool.exe").with_args(["a", "b"]);
    /// assert_eq!(program.args(), &["a", "b"]);
    /// ```
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the configured Windows working directory, when present.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Tool", "tool.exe");
    /// assert_eq!(program.working_directory(), None);
    /// ```
    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// Reports whether launch requests create a new console.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::ProgramSpec;
    ///
    /// let program = ProgramSpec::new("Tool", "tool.exe");
    /// assert!(!program.new_console());
    /// ```
    pub fn new_console(&self) -> bool {
        self.new_console
    }
}
