use bottles_core::layers::{LayerManager, LayerRef, Tools};
use std::fs;
use std::path::PathBuf;

const BASE_REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to REGISTRY\\\\Machine\n\n[Software\\\\Bottles\\\\Base] 1742032912\n#time=1db959146b5541a\n\"Existing\"=\"keepme\"\n\n[Software\\\\ToDelete] 1742032912\n#time=1db959146b5541a\n\"x\"=\"y\"\n\n[Software\\\\ToUpdate] 1742032912\n#time=1db959146b5541a\n\"Ver\"=\"1.0\"\n";

const POST_REG: &str = "WINE REGISTRY Version 2\n;; All keys relative to REGISTRY\\\\Machine\n\n[Software\\\\Bottles\\\\Base] 1742032912\n#time=1db959146b5541a\n\"Existing\"=\"keepme\"\n\n[Software\\\\ToUpdate] 1742032912\n#time=1db959146b5541a\n\"Ver\"=\"2.0\"\n\n[Software\\\\NewDep] 1742032912\n#time=1db959146b5541a\n\"Installed\"=\"yes\"\n";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let work = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("layers-demo"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;

    let mgr = LayerManager::new(Tools::from_env());

    // Virgo base: a file plus a baseline registry.
    let virgo = work.join("virgo");
    fs::create_dir_all(virgo.join("system32"))?;
    fs::write(virgo.join("system32/core.dll"), b"core")?;
    fs::write(virgo.join("system.reg"), BASE_REG)?;
    mgr.commit_layer(&virgo, "virgo")?;

    // --- CAPTURE: install a dependency through a writable prefix ---
    let dep = work.join("dep");
    {
        let mount = mgr.prepare(&[LayerRef::head(&virgo)], &dep, &work.join("mnt1"))?;
        // WineBridge would do this; here we write the install through the mount.
        fs::write(mount.path().join("system32/newdep.dll"), b"newdep")?;
        fs::write(mount.path().join("system.reg"), POST_REG)?;
    } // mount dropped (unmounted); upper `dep` now holds the install
    mgr.capture(&dep, &virgo, "dep")?;
    let dep_patch = dep.join(".fvs2/registry/system.reg.patch");
    println!("captured dep layer files : {:?}", list(&dep));
    println!("captured registry patch  : {} ops", dep_patch.exists().then(|| count_ops(&dep_patch)).unwrap_or(0));

    // --- REPLAY: mount virgo + dep over a fresh upper (patches auto-discovered) ---
    let upper2 = work.join("upper2");
    let mount = mgr.prepare(
        &[LayerRef::head(&virgo), LayerRef::head(&dep)],
        &upper2,
        &work.join("mnt2"),
    )?;
    let merged = fs::read_to_string(mount.path().join("system.reg"))?;
    println!("replay NewDep present    : {}", merged.contains("NewDep"));
    println!("replay ToUpdate Ver2.0   : {}", merged.contains("\"Ver\"=\"2.0\""));
    println!("replay ToDelete gone     : {}", !merged.contains("ToDelete"));
    println!("replay newdep.dll        : {:?}", fs::read_to_string(mount.path().join("system32/newdep.dll"))?);
    println!("replay core.dll (virgo)  : {:?}", fs::read_to_string(mount.path().join("system32/core.dll"))?);
    println!("merged in upper2         : {}", upper2.join("system.reg").exists());
    let base = fs::read_to_string(virgo.join("system.reg"))?;
    println!("virgo untouched          : {}", base.contains("\"Ver\"=\"1.0\"") && base.contains("ToDelete"));

    drop(mount);
    println!("OK");
    Ok(())
}

fn list(dir: &std::path::Path) -> Vec<String> {
    let mut out = vec![];
    for e in walkdir(dir) {
        out.push(e.strip_prefix(dir).unwrap().display().to_string());
    }
    out.sort();
    out
}

fn walkdir(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".fvs2") {
                continue;
            }
            if p.is_dir() {
                out.extend(walkdir(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn count_ops(patch: &std::path::Path) -> usize {
    fs::read_to_string(patch)
        .map(|s| s.lines().filter(|l| l.starts_with('[')).count())
        .unwrap_or(0)
}
