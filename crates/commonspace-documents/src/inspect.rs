//! Read-only filesystem inspection: listings, file typing, duplicate
//! detection. No mutation; no journal entries.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One entry in a directory listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size_bytes: u64,
    /// Best-effort MIME type from magic bytes, falling back to extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// RFC 3339 modification time, when the platform provides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

/// A (possibly truncated) directory listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirListing {
    pub root: PathBuf,
    pub entries: Vec<DirEntryInfo>,
    /// True when `max_entries` cut the listing short.
    pub truncated: bool,
}

/// List a directory tree up to `max_depth` levels and `max_entries` entries.
/// Symlinked directories are not followed (scope safety is checked per-path
/// by callers; not following avoids cycles and surprise traversals).
pub fn list_dir(root: &Path, max_depth: usize, max_entries: usize) -> std::io::Result<DirListing> {
    let mut entries = Vec::new();
    let mut truncated = false;

    for entry in walkdir::WalkDir::new(root)
        .min_depth(1)
        .max_depth(max_depth)
        .follow_links(false)
        .sort_by_file_name()
    {
        if entries.len() >= max_entries {
            truncated = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable entries are skipped, not fatal
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = meta
            .modified()
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from)
            .map(|t| t.to_rfc3339());
        let mime = if meta.is_file() {
            detect_mime(entry.path())
        } else {
            None
        };
        entries.push(DirEntryInfo {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_path_buf(),
            is_dir: meta.is_dir(),
            size_bytes: meta.len(),
            mime,
            modified,
        });
    }

    Ok(DirListing {
        root: root.to_path_buf(),
        entries,
        truncated,
    })
}

/// Detect MIME type: magic bytes first (reads up to 8 KiB), extension as
/// fallback. Returns `None` when neither yields anything.
pub fn detect_mime(path: &Path) -> Option<String> {
    if let Ok(Some(kind)) = infer::get_from_path(path) {
        return Some(kind.mime_type().to_string());
    }
    mime_guess::from_path(path)
        .first()
        .map(|m| m.essence_str().to_string())
}

/// BLAKE3 hash of a file's contents, hex-encoded. Streams in chunks so
/// large files do not load into memory.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Find duplicate files under `root`: groups of 2+ paths with identical
/// content. Files are pre-grouped by size so only same-size candidates are
/// hashed.
pub fn find_duplicates(
    root: &Path,
    max_files: usize,
) -> std::io::Result<Vec<(String, Vec<PathBuf>)>> {
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut seen = 0usize;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        seen += 1;
        if seen > max_files {
            break;
        }
        by_size
            .entry(meta.len())
            .or_default()
            .push(entry.path().to_path_buf());
    }

    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    for (_, candidates) in by_size.into_iter().filter(|(_, v)| v.len() > 1) {
        let mut by_hash: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in candidates {
            if let Ok(hash) = hash_file(&path) {
                by_hash.entry(hash).or_default().push(path);
            }
        }
        for (hash, paths) in by_hash.into_iter().filter(|(_, v)| v.len() > 1) {
            groups.push((hash, paths));
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(groups)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn lists_with_metadata_and_mime() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("naïve résumé.md"), "# hi").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("b.txt"), "b").unwrap();
        let listing = list_dir(tmp.path(), 3, 100).unwrap();
        assert_eq!(listing.entries.len(), 3);
        assert!(!listing.truncated);
        let md = listing
            .entries
            .iter()
            .find(|e| e.name.contains("résumé"))
            .unwrap();
        assert!(!md.is_dir);
        assert!(md.modified.is_some());
    }

    #[test]
    fn truncation_reported() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(tmp.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let listing = list_dir(tmp.path(), 1, 5).unwrap();
        assert_eq!(listing.entries.len(), 5);
        assert!(listing.truncated);
    }

    #[test]
    fn duplicates_found_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "same-content").unwrap();
        std::fs::write(tmp.path().join("copy of a.txt"), "same-content").unwrap();
        std::fs::write(tmp.path().join("different.txt"), "other").unwrap();
        // Same size, different content — must not group.
        std::fs::write(tmp.path().join("x.bin"), "aaaaaaaaaaaa").unwrap();
        std::fs::write(tmp.path().join("y.bin"), "bbbbbbbbbbbb").unwrap();
        let groups = find_duplicates(tmp.path(), 1000).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1.len(), 2);
    }
}
