//! Orchestration of layered Wine prefixes ("Next mode").
//!
//! A prefix is a stack of immutable FVS layers (a vanilla "Virgo" base plus one
//! layer per installed dependency/application) mounted read-only through the
//! `fvs2d` FUSE daemon, with an optional writable upper where runtime changes
//! and freshly captured installs land. File changes are captured as the upper
//! directory itself; registry changes are captured as a compact `regdiff` patch
//! per layer and re-applied into the upper at mount time, so the base stays
//! untouched.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use thiserror::Error;

use crate::runner::{PrefixConfig, Runner};
use crate::winebridge::{LaunchRequest, WineBridgeClient};

#[derive(Error, Debug)]
pub enum LayersError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("spawning {tool}: {source}")]
    Spawn {
        tool: String,
        source: std::io::Error,
    },
    #[error("{tool} failed (code {code}): {stderr}")]
    Tool {
        tool: String,
        code: i32,
        stderr: String,
    },
    #[error("mount {0} did not become ready")]
    MountTimeout(PathBuf),
}

pub type Result<T> = std::result::Result<T, LayersError>;

/// Paths to the external tools the manager drives.
#[derive(Clone, Debug)]
pub struct Tools {
    pub fvs2d: PathBuf,
    pub fvs2: PathBuf,
    pub regdiff: PathBuf,
}

impl Tools {
    /// Reads tool paths from `FVS2D_BIN`, `FVS2_BIN`, `REGDIFF_BIN`, falling back
    /// to the bare names resolved through `PATH`.
    pub fn from_env() -> Self {
        let pick = |var: &str, default: &str| {
            std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(default))
        };
        Self {
            fvs2d: pick("FVS2D_BIN", "fvs2d"),
            fvs2: pick("FVS2_BIN", "fvs2"),
            regdiff: pick("REGDIFF_BIN", "regdiff"),
        }
    }
}

/// A reference to one committed FVS layer.
#[derive(Clone, Debug)]
pub struct LayerRef {
    pub repo: PathBuf,
    pub state: Option<String>,
}

impl LayerRef {
    pub fn head(repo: impl Into<PathBuf>) -> Self {
        Self { repo: repo.into(), state: None }
    }

    pub fn state(repo: impl Into<PathBuf>, state: impl Into<String>) -> Self {
        Self { repo: repo.into(), state: Some(state.into()) }
    }

    fn to_arg(&self) -> String {
        match &self.state {
            Some(s) => format!("{}@{}", self.repo.display(), s),
            None => self.repo.display().to_string(),
        }
    }
}

/// A live FUSE mount. Unmounts and reaps the daemon on drop.
pub struct Mount {
    child: Option<Child>,
    mountpoint: PathBuf,
}

impl Mount {
    pub fn path(&self) -> &Path {
        &self.mountpoint
    }

    pub fn unmount(&mut self) {
        let _ = Command::new("fusermount3").arg("-u").arg(&self.mountpoint).status();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        self.unmount();
    }
}

/// A running layered session: the mounted prefix, its WineBridge agent, and the
/// pid of the launched application. The prefix is unmounted when the session is
/// dropped (after the bridge is shut down).
pub struct Session {
    pub mount: Mount,
    pub bridge: WineBridgeClient,
    pub pid: u32,
}

pub struct LayerManager {
    tools: Tools,
}

impl LayerManager {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }

    /// Mounts a stack of layers (low to high) at `mountpoint`, optionally backed
    /// by a writable `upper`. Returns once the mount is visible.
    pub fn mount(&self, lowers: &[LayerRef], upper: Option<&Path>, mountpoint: &Path) -> Result<Mount> {
        std::fs::create_dir_all(mountpoint)?;
        let mut cmd = Command::new(&self.tools.fvs2d);
        cmd.arg("-mount").arg(mountpoint);
        for l in lowers {
            cmd.arg("-lower").arg(l.to_arg());
        }
        if let Some(u) = upper {
            std::fs::create_dir_all(u)?;
            cmd.arg("-upper").arg(u);
        }
        let child = cmd.spawn().map_err(|e| LayersError::Spawn { tool: "fvs2d".into(), source: e })?;
        let mount = Mount { child: Some(child), mountpoint: mountpoint.to_path_buf() };
        wait_mounted(mountpoint)?;
        Ok(mount)
    }

    /// Applies one registry patch onto a target registry file in place.
    fn apply_one(&self, target: &Path, patch: &Path, hive: &str) -> Result<()> {
        self.run(
            &self.tools.regdiff,
            None,
            [
                OsStr::new("apply"),
                target.as_os_str(),
                patch.as_os_str(),
                target.as_os_str(),
                OsStr::new(hive),
            ],
            "regdiff",
        )
    }

    /// Diffs a base registry against a modified one into a compact patch.
    fn diff_one(&self, base: &Path, modified: &Path, out_patch: &Path, hive: &str) -> Result<()> {
        self.run(
            &self.tools.regdiff,
            None,
            [
                OsStr::new("diff"),
                base.as_os_str(),
                modified.as_os_str(),
                out_patch.as_os_str(),
                OsStr::new(hive),
            ],
            "regdiff",
        )
    }

    /// Commits a directory as a new FVS state, returning nothing (the layer is
    /// the directory's `.fvs2` repo at its new HEAD).
    pub fn commit_layer(&self, dir: &Path, message: &str) -> Result<()> {
        let _ = self.run(&self.tools.fvs2, Some(dir), ["init"], "fvs2");
        self.run(&self.tools.fvs2, Some(dir), ["commit", "-m", message], "fvs2")
    }

    /// Mounts the stack over a writable upper and applies each layer's stored
    /// registry patches (low to high) onto the prefix registries, materialising
    /// the merged registries in the upper. Base layers are never modified.
    pub fn prepare(&self, lowers: &[LayerRef], upper: &Path, mountpoint: &Path) -> Result<Mount> {
        let mount = self.mount(lowers, Some(upper), mountpoint)?;
        for layer in lowers {
            let dir = registry_patch_dir(&layer.repo);
            for (reg, hive) in REG_FILES {
                let patch = dir.join(format!("{reg}.patch"));
                let target = mount.path().join(reg);
                if patch.exists() && target.exists() {
                    self.apply_one(&target, &patch, hive)?;
                }
            }
        }
        Ok(mount)
    }

    /// Captures the writable upper of a finished session as a new layer. Each
    /// prefix registry is reduced to a patch against the matching file in
    /// `base_dir` (the registry state the layer was installed over), stored as
    /// layer metadata under `.fvs2/registry/` and removed from the tree; then the
    /// upper is committed as a new FVS state.
    pub fn capture(&self, upper: &Path, base_dir: &Path, message: &str) -> Result<()> {
        let _ = self.run(&self.tools.fvs2, Some(upper), ["init"], "fvs2");
        let patch_dir = registry_patch_dir(upper);
        std::fs::create_dir_all(&patch_dir)?;
        for (reg, hive) in REG_FILES {
            let merged = upper.join(reg);
            let base = base_dir.join(reg);
            if merged.exists() && base.exists() {
                self.diff_one(&base, &merged, &patch_dir.join(format!("{reg}.patch")), hive)?;
                std::fs::remove_file(&merged)?;
            }
        }
        self.run(&self.tools.fvs2, Some(upper), ["commit", "-m", message], "fvs2")
    }

    /// Prepares a layered prefix and launches an application in it through
    /// WineBridge. Returns the live [`Session`]; once the app exits the caller
    /// may [`capture`](Self::capture) the upper as a new layer.
    pub async fn prepare_and_launch(
        &self,
        runner: &dyn Runner,
        winebridge_executable: PathBuf,
        lowers: &[LayerRef],
        upper: &Path,
        mountpoint: &Path,
        request: LaunchRequest,
    ) -> crate::error::Result<Session> {
        let mount = self.prepare(lowers, upper, mountpoint)?;
        let prefix = PrefixConfig::builder().path(mount.path())?.build()?;
        let bridge = WineBridgeClient::new(runner, &prefix, winebridge_executable).await?;
        let pid = bridge.launch_process(request).await?;
        Ok(Session { mount, bridge, pid })
    }

    fn run<I, S>(&self, bin: &Path, dir: Option<&Path>, args: I, tool: &str) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = Command::new(bin);
        if let Some(d) = dir {
            cmd.current_dir(d);
        }
        cmd.args(args);
        let out = cmd.output().map_err(|e| LayersError::Spawn { tool: tool.into(), source: e })?;
        if !out.status.success() {
            return Err(LayersError::Tool {
                tool: tool.into(),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(())
    }
}

/// The Wine prefix registry files and the hive each is diffed/applied under.
const REG_FILES: &[(&str, &str)] = &[
    ("system.reg", "hklm"),
    ("user.reg", "hkcu"),
    ("userdef.reg", "hkcu"),
];

/// Where a layer keeps its registry patches: under the FVS metadata dir, which
/// is excluded from the mounted prefix tree.
fn registry_patch_dir(repo: &Path) -> PathBuf {
    repo.join(".fvs2").join("registry")
}

fn wait_mounted(mountpoint: &Path) -> Result<()> {
    let target = std::fs::canonicalize(mountpoint)?;
    for _ in 0..60 {
        if is_mounted(&target) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(LayersError::MountTimeout(mountpoint.to_path_buf()))
}

fn is_mounted(target: &Path) -> bool {
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    let target = target.to_string_lossy();
    info.lines().any(|line| line.split(' ').nth(4) == Some(target.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to REGISTRY\\\\Machine\n\n[Software\\\\ToDelete] 1742032912\n#time=1db959146b5541a\n\"x\"=\"y\"\n\n[Software\\\\ToUpdate] 1742032912\n#time=1db959146b5541a\n\"Ver\"=\"1.0\"\n";
    const POST_REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to REGISTRY\\\\Machine\n\n[Software\\\\ToUpdate] 1742032912\n#time=1db959146b5541a\n\"Ver\"=\"2.0\"\n\n[Software\\\\NewDep] 1742032912\n#time=1db959146b5541a\n\"Installed\"=\"yes\"\n";

    /// Returns the tools only when their paths are explicitly configured, so the
    /// test is a no-op where fvs2d/fvs2/regdiff (and FUSE) are unavailable.
    fn configured_tools() -> Option<Tools> {
        let set = |v: &str| std::env::var_os(v).is_some();
        (set("FVS2D_BIN") && set("FVS2_BIN") && set("REGDIFF_BIN")).then(Tools::from_env)
    }

    #[test]
    fn capture_then_replay() {
        let Some(tools) = configured_tools() else {
            eprintln!("skipping capture_then_replay: set FVS2D_BIN/FVS2_BIN/REGDIFF_BIN");
            return;
        };
        let mgr = LayerManager::new(tools);
        let work = std::env::temp_dir().join(format!("layers-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();

        let virgo = work.join("virgo");
        std::fs::create_dir_all(virgo.join("system32")).unwrap();
        std::fs::write(virgo.join("system32/core.dll"), b"core").unwrap();
        std::fs::write(virgo.join("system.reg"), BASE_REG).unwrap();
        mgr.commit_layer(&virgo, "virgo").unwrap();

        let dep = work.join("dep");
        {
            let mount = mgr.prepare(&[LayerRef::head(&virgo)], &dep, &work.join("mnt1")).unwrap();
            std::fs::write(mount.path().join("system32/newdep.dll"), b"newdep").unwrap();
            std::fs::write(mount.path().join("system.reg"), POST_REG).unwrap();
        }
        mgr.capture(&dep, &virgo, "dep").unwrap();
        assert!(registry_patch_dir(&dep).join("system.reg.patch").exists());

        let mount = mgr
            .prepare(&[LayerRef::head(&virgo), LayerRef::head(&dep)], &work.join("upper2"), &work.join("mnt2"))
            .unwrap();
        let merged = std::fs::read_to_string(mount.path().join("system.reg")).unwrap();
        assert!(merged.contains("NewDep"), "merged registry should gain NewDep");
        assert!(merged.contains("\"Ver\"=\"2.0\""), "ToUpdate should be 2.0");
        assert!(!merged.contains("ToDelete"), "ToDelete should be whiteed out");
        assert_eq!(std::fs::read_to_string(mount.path().join("system32/newdep.dll")).unwrap(), "newdep");

        let base = std::fs::read_to_string(virgo.join("system.reg")).unwrap();
        assert!(base.contains("\"Ver\"=\"1.0\"") && base.contains("ToDelete"), "virgo must stay untouched");

        drop(mount);
        let _ = std::fs::remove_dir_all(&work);
    }
}
