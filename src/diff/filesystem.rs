use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_secs: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffOp {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub operation: DiffOp,
}

pub struct FilesystemDiff;

impl FilesystemDiff {
    /// Walks a directory tree and returns metadata for every entry.
    pub fn snapshot(root: &Path) -> Vec<FileEntry> {
        WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");

                if relative.is_empty() {
                    return None;
                }

                let modified_secs = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                Some(FileEntry {
                    path: relative,
                    is_dir: meta.is_dir(),
                    size: meta.len(),
                    modified_secs,
                })
            })
            .collect()
    }

    /// Computes the diff between two filesystem snapshots.
    pub fn diff(before: &[FileEntry], after: &[FileEntry]) -> Vec<FileDiff> {
        let before_map: HashMap<&str, &FileEntry> =
            before.iter().map(|e| (e.path.as_str(), e)).collect();
        let after_map: HashMap<&str, &FileEntry> =
            after.iter().map(|e| (e.path.as_str(), e)).collect();

        let mut changes = Vec::new();

        for (path, b) in &before_map {
            match after_map.get(path) {
                None => changes.push(FileDiff {
                    path: path.to_string(),
                    operation: DiffOp::Removed,
                }),
                Some(a)
                    if !b.is_dir
                        && (a.size != b.size || a.modified_secs != b.modified_secs) =>
                {
                    changes.push(FileDiff {
                        path: path.to_string(),
                        operation: DiffOp::Modified,
                    })
                }
                _ => {}
            }
        }

        for path in after_map.keys() {
            if !before_map.contains_key(path) {
                changes.push(FileDiff {
                    path: path.to_string(),
                    operation: DiffOp::Added,
                });
            }
        }

        changes
    }
}
