//! Detection and provisioning for Apple's Game Porting Toolkit (GPTK).
//!
//! GPTK's Wine build (`wine64`) is x86_64-only, so Apple Silicon needs Rosetta 2
//! and a dedicated x86_64 Homebrew prefix at `/usr/local`; Intel Macs need
//! neither, since `/usr/local` is already their native prefix. [`status`] only
//! inspects the system. The `install_*` functions shell out to provision one
//! missing piece each and never run implicitly.

use std::path::PathBuf;

use async_process::Command;
use thiserror::Error;

use crate::error::Result;

/// The x86_64 Homebrew prefix GPTK requires on both Intel and Apple Silicon.
///
/// Apple Silicon's native Homebrew lives at `/opt/homebrew`; GPTK and its Wine
/// build must run under Rosetta 2 from the Intel prefix instead.
const X86_64_BREW: &str = "/usr/local/bin/brew";

#[derive(Debug, Error)]
pub enum GptkSetupError {
    #[error("Rosetta 2 installation failed")]
    RosettaInstallFailed,
    #[error("x86_64 Homebrew installation failed")]
    HomebrewInstallFailed,
    #[error("GPTK installation failed")]
    GptkInstallFailed,
    #[error("x86_64 Homebrew was not found at {0}")]
    HomebrewNotFound(PathBuf),
    #[error("installed GPTK's location could not be resolved")]
    GptkPathUnresolved,
}

/// A point-in-time read of GPTK's prerequisites and installation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct GptkStatus {
    pub apple_silicon: bool,
    pub rosetta_installed: bool,
    pub x86_64_homebrew_installed: bool,
    pub gptk_installed: bool,
}

/// Inspects Rosetta 2, x86_64 Homebrew, and GPTK without changing anything.
pub async fn status() -> GptkStatus {
    GptkStatus {
        apple_silicon: is_apple_silicon(),
        rosetta_installed: has_rosetta().await,
        x86_64_homebrew_installed: has_x86_64_homebrew().await,
        gptk_installed: gptk_path().await.is_some(),
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

async fn has_x86_64_homebrew() -> bool {
    async_fs::metadata(X86_64_BREW)
        .await
        .is_ok_and(|entry| entry.is_file())
}

/// Resolves the installed GPTK component directory, if any, by asking the
/// x86_64 Homebrew for its formula prefix and confirming `bin/wine64` exists.
pub async fn gptk_path() -> Option<PathBuf> {
    let output = Command::new(X86_64_BREW)
        .arg("--prefix")
        .arg("game-porting-toolkit")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    async_fs::metadata(path.join("bin/wine64"))
        .await
        .is_ok()
        .then_some(path)
}

/// Runs Apple's Rosetta 2 installer.
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

/// Runs Homebrew's official installer script under Rosetta 2, producing the
/// x86_64 prefix at `/usr/local`.
pub async fn install_x86_64_homebrew() -> Result<()> {
    let status = Command::new("arch")
        .arg("-x86_64")
        .arg("/bin/bash")
        .arg("-c")
        .arg(
            "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"",
        )
        .status()
        .await?;
    if !status.success() {
        return Err(GptkSetupError::HomebrewInstallFailed.into());
    }
    Ok(())
}

/// Installs GPTK through the x86_64 Homebrew's `gcenx/wine` tap and returns its
/// installed component directory.
///
/// # Errors
///
/// Returns [`GptkSetupError::HomebrewNotFound`] when the x86_64 Homebrew is not
/// yet installed.
pub async fn install_gptk() -> Result<PathBuf> {
    if !has_x86_64_homebrew().await {
        return Err(GptkSetupError::HomebrewNotFound(PathBuf::from(X86_64_BREW)).into());
    }
    let status = Command::new("arch")
        .arg("-x86_64")
        .arg(X86_64_BREW)
        .arg("install")
        .arg("gcenx/wine/game-porting-toolkit")
        .status()
        .await?;
    if !status.success() {
        return Err(GptkSetupError::GptkInstallFailed.into());
    }
    gptk_path()
        .await
        .ok_or_else(|| GptkSetupError::GptkPathUnresolved.into())
}
