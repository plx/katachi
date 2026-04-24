//! Temp overlay helper for materialized runs.
//!
//! A [`TempOverlay`] wraps a [`tempfile::TempDir`] and exposes helpers for
//! writing inline content, copying files, and creating symlinks inside the
//! overlay, tracking a manifest of every action.
//!
//! The overlay defaults to dropping on success (via `TempDir`'s normal
//! cleanup) and **keeping on failure** — callers signal failure via
//! [`TempOverlay::keep`] or [`TempOverlay::promote`] before the value is
//! dropped. This matches the "preserve failed run overlays by default"
//! guidance in the katachi design docs.

use std::fs;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use tempfile::{Builder, TempDir};

/// An ephemeral filesystem overlay rooted in a `TempDir`.
pub struct TempOverlay {
    root: Utf8PathBuf,
    /// `None` once the tempdir has been promoted (leaked into a stable path)
    /// or detached for manual cleanup.
    tempdir: Option<TempDir>,
    manifest: Vec<OverlayEntry>,
    keep_policy: KeepPolicy,
}

/// Whether to preserve the overlay on drop.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepPolicy {
    /// Always remove the overlay when the `TempOverlay` is dropped.
    Discard,
    /// Keep the overlay (leak the `TempDir`) on drop.
    Keep,
}

/// A single recorded action inside the overlay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayEntry {
    pub dest: Utf8PathBuf,
    #[serde(flatten)]
    pub kind: OverlayEntryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayEntryKind {
    Inline { bytes: u64 },
    CopyFrom { from: Utf8PathBuf, bytes: u64 },
    SymlinkTo { target: Utf8PathBuf },
}

impl TempOverlay {
    /// Create a new overlay with the default prefix `katachi-overlay-`.
    pub fn new() -> io::Result<Self> {
        Self::with_prefix("katachi-overlay-")
    }

    /// Create a new overlay with a custom tempdir prefix.
    pub fn with_prefix(prefix: &str) -> io::Result<Self> {
        let tempdir = Builder::new().prefix(prefix).tempdir()?;
        let root = Utf8PathBuf::from_path_buf(tempdir.path().to_path_buf()).map_err(|p| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("overlay root is not UTF-8: {}", p.display()),
            )
        })?;
        Ok(Self {
            root,
            tempdir: Some(tempdir),
            manifest: Vec::new(),
            keep_policy: KeepPolicy::Discard,
        })
    }

    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    pub fn manifest(&self) -> &[OverlayEntry] {
        &self.manifest
    }

    /// Switch the keep policy. Call [`KeepPolicy::Keep`] before dropping
    /// the overlay to preserve its contents.
    pub fn set_keep(&mut self, policy: KeepPolicy) {
        self.keep_policy = policy;
    }

    /// Write `contents` into `rel` inside the overlay, creating parents as
    /// needed.
    pub fn write_inline(
        &mut self,
        rel: impl AsRef<Utf8Path>,
        contents: &str,
    ) -> io::Result<Utf8PathBuf> {
        let rel = rel.as_ref();
        let abs = self.resolve_rel(rel)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&abs, contents.as_bytes())?;
        self.manifest.push(OverlayEntry {
            dest: rel.to_owned(),
            kind: OverlayEntryKind::Inline {
                bytes: contents.len() as u64,
            },
        });
        Ok(abs)
    }

    /// Copy `src` into the overlay at `rel`.
    pub fn copy_from(
        &mut self,
        rel: impl AsRef<Utf8Path>,
        src: &Utf8Path,
    ) -> io::Result<Utf8PathBuf> {
        let rel = rel.as_ref();
        let abs = self.resolve_rel(rel)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = fs::copy(src, &abs)?;
        self.manifest.push(OverlayEntry {
            dest: rel.to_owned(),
            kind: OverlayEntryKind::CopyFrom {
                from: src.to_owned(),
                bytes,
            },
        });
        Ok(abs)
    }

    /// Create a symlink at `rel` pointing at `target`.
    #[cfg(unix)]
    pub fn symlink(
        &mut self,
        rel: impl AsRef<Utf8Path>,
        target: &Utf8Path,
    ) -> io::Result<Utf8PathBuf> {
        let rel = rel.as_ref();
        let abs = self.resolve_rel(rel)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(target, &abs)?;
        self.manifest.push(OverlayEntry {
            dest: rel.to_owned(),
            kind: OverlayEntryKind::SymlinkTo {
                target: target.to_owned(),
            },
        });
        Ok(abs)
    }

    #[cfg(not(unix))]
    pub fn symlink(
        &mut self,
        _rel: impl AsRef<Utf8Path>,
        _target: &Utf8Path,
    ) -> io::Result<Utf8PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlink overlays are only supported on Unix-like platforms",
        ))
    }

    /// Move the overlay to a stable path and return the new root.
    ///
    /// After promotion the overlay no longer owns the tempdir — the
    /// destination persists independently of this handle.
    pub fn promote(mut self, dest: &Utf8Path) -> io::Result<Utf8PathBuf> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(td) = self.tempdir.take() {
            let src_path = td.keep(); // dissolve TempDir but keep the files
            fs::rename(&src_path, dest.as_std_path()).or_else(|_| {
                // Cross-filesystem rename — fall back to copy+remove.
                copy_dir_recursive(
                    Utf8Path::from_path(&src_path).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "overlay path is not UTF-8")
                    })?,
                    dest,
                )?;
                fs::remove_dir_all(&src_path)?;
                Ok::<(), io::Error>(())
            })?;
        }
        Ok(dest.to_owned())
    }

    fn resolve_rel(&self, rel: &Utf8Path) -> io::Result<Utf8PathBuf> {
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, camino::Utf8Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("overlay path `{rel}` must be relative and must not contain `..`"),
            ));
        }
        Ok(self.root.join(rel))
    }
}

impl Drop for TempOverlay {
    fn drop(&mut self) {
        if matches!(self.keep_policy, KeepPolicy::Keep) {
            if let Some(td) = self.tempdir.take() {
                // Leak the tempdir so its contents survive the drop.
                let _ = td.keep();
            }
        }
        // Otherwise: TempDir's own Drop cleans up.
    }
}

fn copy_dir_recursive(src: &Utf8Path, dst: &Utf8Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dest_path = dst.join(
            entry
                .file_name()
                .to_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 path"))?,
        );
        let src_utf8 = Utf8Path::from_path(&src_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 path in overlay")
        })?;
        if file_type.is_dir() {
            copy_dir_recursive(src_utf8, &dest_path)?;
        } else if file_type.is_file() {
            fs::copy(src_utf8, &dest_path)?;
        } else if file_type.is_symlink() {
            let target = fs::read_link(&src_path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dest_path)?;
            #[cfg(not(unix))]
            let _ = target;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_inline_creates_parents_and_records_manifest() {
        let mut ov = TempOverlay::new().unwrap();
        ov.write_inline("project/.claude/CLAUDE.md", "hello")
            .unwrap();
        let abs = ov.root().join("project/.claude/CLAUDE.md");
        assert_eq!(fs::read_to_string(abs).unwrap(), "hello");
        let m = ov.manifest();
        assert_eq!(m.len(), 1);
        match &m[0].kind {
            OverlayEntryKind::Inline { bytes } => assert_eq!(*bytes, 5),
            _ => panic!("wrong manifest kind"),
        }
    }

    #[test]
    fn copy_from_records_source() {
        let src_dir = tempfile::tempdir().unwrap();
        let src_path = Utf8PathBuf::from_path_buf(src_dir.path().join("src.txt")).unwrap();
        fs::write(src_path.as_std_path(), "abc").unwrap();

        let mut ov = TempOverlay::new().unwrap();
        ov.copy_from("copied.txt", &src_path).unwrap();
        let m = ov.manifest();
        assert_eq!(m.len(), 1);
        match &m[0].kind {
            OverlayEntryKind::CopyFrom { from, bytes } => {
                assert_eq!(from, &src_path);
                assert_eq!(*bytes, 3);
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn symlink_records_target() {
        let mut ov = TempOverlay::new().unwrap();
        let target = Utf8PathBuf::from("/tmp/nonexistent-target-ok");
        ov.symlink("link", &target).unwrap();
        let m = ov.manifest();
        assert_eq!(m.len(), 1);
        match &m[0].kind {
            OverlayEntryKind::SymlinkTo { target: t } => assert_eq!(t, &target),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn rejects_escape_via_parent_dir() {
        let mut ov = TempOverlay::new().unwrap();
        let err = ov.write_inline("../escape.txt", "no").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_absolute_destination() {
        let mut ov = TempOverlay::new().unwrap();
        let err = ov.write_inline("/absolute/path", "no").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn drop_discards_by_default() {
        let path = {
            let ov = TempOverlay::new().unwrap();
            ov.root().to_owned()
        };
        assert!(
            !path.exists(),
            "overlay should be cleaned up on drop by default"
        );
    }

    #[test]
    fn keep_policy_preserves_on_drop() {
        let path = {
            let mut ov = TempOverlay::new().unwrap();
            ov.set_keep(KeepPolicy::Keep);
            ov.root().to_owned()
        };
        assert!(path.exists(), "overlay should survive drop when kept");
        // Clean up after ourselves.
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn promote_moves_contents_to_stable_path() {
        let parent = tempfile::tempdir().unwrap();
        let dest = Utf8PathBuf::from_path_buf(parent.path().join("promoted")).unwrap();

        let mut ov = TempOverlay::new().unwrap();
        ov.write_inline("hello.txt", "hi").unwrap();
        let new_root = ov.promote(&dest).unwrap();
        assert_eq!(new_root, dest);
        assert!(dest.join("hello.txt").exists());
        assert_eq!(fs::read_to_string(dest.join("hello.txt")).unwrap(), "hi");
    }
}
