//! Rosetta 2 provisioning for Apple's Game Porting Toolkit.
//!
//! GPTK is a catalog runner component, downloaded and extracted like any other
//! runner, and its Wine layout carries every library it links against. The one
//! thing the host must still provide is Rosetta 2: `wine64` and its bundled
//! dylibs are x86_64-only, so every Wine process runs translated on Apple
//! Silicon. Intel Macs need nothing extra.
//!
//! [`status`] only inspects the system. [`install_rosetta`] shells out to
//! Apple's installer and never runs implicitly.

use async_process::Command;
use thiserror::Error;

use crate::error::Result;

#[derive(Debug, Error)]
pub enum GptkSetupError {
    #[error("Rosetta 2 installation failed")]
    RosettaInstallFailed,
}

/// A point-in-time read of GPTK's host prerequisite.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct GptkStatus {
    pub apple_silicon: bool,
    pub rosetta_installed: bool,
}

impl GptkStatus {
    /// Whether this host can run a GPTK runner, i.e. it is an Intel Mac or an
    /// Apple Silicon Mac with Rosetta 2 installed.
    pub fn ready(&self) -> bool {
        !self.apple_silicon || self.rosetta_installed
    }
}

/// Inspects Rosetta 2 without changing anything.
pub async fn status() -> GptkStatus {
    GptkStatus {
        apple_silicon: is_apple_silicon(),
        rosetta_installed: has_rosetta().await,
    }
}

fn is_apple_silicon() -> bool {
    std::env::consts::ARCH == "aarch64"
}

async fn has_rosetta() -> bool {
    async_fs::metadata("/Library/Apple/usr/share/rosetta/rosetta")
        .await
        .is_ok()
}

/// Runs Apple's Rosetta 2 installer.
///
/// # Errors
///
/// Returns [`GptkSetupError::RosettaInstallFailed`] when the installer exits
/// unsuccessfully.
pub async fn install_rosetta() -> Result<()> {
    let status = Command::new("softwareupdate")
        .arg("--install-rosetta")
        .arg("--agree-to-license")
        .status()
        .await?;
    if !status.success() {
        return Err(GptkSetupError::RosettaInstallFailed.into());
    }
    Ok(())
}
