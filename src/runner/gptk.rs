use super::{Runner, RunnerInfo};
use std::path::{Path, PathBuf};

/// GPTK (Game Porting Toolkit) runner for macOS
///
/// GPTK is Apple's translation layer that allows running Windows games on macOS,
/// particularly on Apple Silicon Macs. It combines Wine with Apple's D3DMetal
/// to support DirectX 11 and 12 games with hardware acceleration.
///
/// # Features
///
/// - DirectX 11 and 12 support through D3DMetal translation
/// - Optimized for Apple Silicon architecture
/// - Integration with macOS graphics stack
/// - Metal Performance Shaders acceleration
/// - Enhanced gaming compatibility on macOS
///
/// # Requirements
/// - macOS 14 Sonoma or later
/// - Apple Silicon Mac (recommended) or Intel Mac with Rosetta 2
/// - Command Line Tools for Xcode 15 or later
/// - Game Porting Toolkit installed via Homebrew
///
/// # Example
/// ```rust
/// use bottles_core::runner::{GPTK, Runner};
/// use std::path::Path;
///
/// // Create a GPTK runner from a path containing the gameportingtoolkit executable
/// let gptk_path = Path::new("/opt/homebrew/bin");
/// match GPTK::try_from(gptk_path) {
///     Ok(gptk) => {
///         println!("GPTK Name: {}", gptk.info().name());
///         println!("GPTK Version: {}", gptk.info().version());
///         println!("GPTK Available: {}", gptk.is_available());
///     }
///     Err(e) => println!("Failed to create GPTK runner: {}", e),
/// }
/// ```
#[derive(Debug)]
pub struct GPTK {
    info: RunnerInfo,
}

impl TryFrom<&Path> for GPTK {
    type Error = crate::Error;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        let executable = PathBuf::from("./gameportingtoolkit");
        let info = RunnerInfo::try_from(path, &executable)?;
        Ok(GPTK { info })
    }
}

impl Runner for GPTK {
    fn wine(&self) -> &super::Wine {
        todo!()
    }

    fn info(&self) -> &RunnerInfo {
        &self.info
    }

    /// GPTK has special availability requirements - it only works on Apple Silicon Macs
    /// running macOS 14 Sonoma or later with Rosetta 2
    fn is_available(&self) -> bool {
        if !self.info().executable_path().exists() {
            return false;
        }

        // Check if running under Rosetta or on Apple Silicon
        use std::process::Command;
        let arch_output = Command::new("arch")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default();

        // GPTK requires either x86_64 (Rosetta) or arm64 (Apple Silicon)
        arch_output == "i386" || arch_output == "arm64"
    }
}
