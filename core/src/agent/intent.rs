//! Trusted operations. Model text is decoded into data and never passed to an interpreter.

use crate::exec::{Limits, Outcome};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intent {
    CreateDirectory { path: String },
    CreateFile { path: String },
    ListDirectory { path: String },
    RenamePath { path: String, name: String },
    MovePath { path: String, destination: String },
    TrashPath { path: String },
}

impl Intent {
    pub fn label(&self) -> String {
        match self {
            Self::CreateDirectory { path } => format!("Create directory: {path}"),
            Self::CreateFile { path } => format!("Create file: {path}"),
            Self::ListDirectory { path } => format!("List directory: {path}"),
            Self::RenamePath { path, name } => format!("Rename: {path} → {name}"),
            Self::MovePath { path, destination } => format!("Move: {path} → {destination}"),
            Self::TrashPath { path } => format!("Move to Trash: {path}"),
        }
    }

    /// Changes or removes something that already exists; the shell asks before running these.
    pub fn destructive(&self) -> bool {
        matches!(self, Self::RenamePath { .. } | Self::MovePath { .. } | Self::TrashPath { .. })
    }

    fn path(&self) -> &str {
        match self {
            Self::CreateDirectory { path }
            | Self::CreateFile { path }
            | Self::ListDirectory { path }
            | Self::RenamePath { path, .. }
            | Self::MovePath { path, .. }
            | Self::TrashPath { path } => path,
        }
    }

    pub fn legacy(command: &str) -> Option<Self> {
        if command.chars().any(|c| ";&|$`<>\\\n\r{}*?".contains(c)) {
            return None;
        }
        let (kind, path) = if let Some(path) = command.strip_prefix("mkdir -p ") {
            (0, path)
        } else if let Some(path) = command.strip_prefix("touch ") {
            (1, path)
        } else if let Some(path) = command.strip_prefix("ls ") {
            (2, path)
        } else {
            return None;
        };
        if path.is_empty()
            || path.starts_with('-')
            || path.contains(['\'', '"'])
            || path.split_whitespace().count() != 1
        {
            return None;
        }
        Some(match kind {
            0 => Self::CreateDirectory { path: path.into() },
            1 => Self::CreateFile { path: path.into() },
            _ => Self::ListDirectory { path: path.into() },
        })
    }

    fn target(&self, cwd: &Path, scope: &[PathBuf], limits: &Limits) -> Result<(PathBuf, PathBuf), String> {
        Self::resolve(self.path(), cwd, scope, limits)
    }

    fn resolve(path: &str, cwd: &Path, scope: &[PathBuf], limits: &Limits) -> Result<(PathBuf, PathBuf), String> {
        if path.is_empty() || path.len() > 4096 || path.contains('\0') {
            return Err("Invalid path".into());
        }
        let path = Path::new(path);
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("Path is outside the allowed directory".into());
        }
        let cwd = cwd.canonicalize().map_err(|_| "Directory unavailable")?;
        let target = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if target.components().any(|c| matches!(c, Component::Normal(name) if [".ssh", ".aws", ".gnupg", ".config", "Library"].iter().any(|p| name.to_string_lossy().eq_ignore_ascii_case(p)))) {
            return Err("Protected directory".into());
        }
        let root = scope
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .find(|root| target.starts_with(root))
            .ok_or("Path is outside the allowed directory")?;
        if !limits
            .roots
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .any(|allowed| root.starts_with(allowed))
        {
            return Err("Path is outside configured roots".into());
        }
        let relative = target
            .strip_prefix(&root)
            .map_err(|_| "Invalid path")?
            .to_path_buf();
        Ok((root, relative))
    }

    pub fn check(&self, cwd: &Path, scope: &[PathBuf], limits: &Limits) -> Result<(), String> {
        let (_, relative) = self.target(cwd, scope, limits)?;
        match self {
            Self::RenamePath { name, .. } => valid_name(name).and_then(|()| inside(&relative)),
            Self::MovePath { destination, .. } => Self::resolve(destination, cwd, scope, limits).and_then(|_| inside(&relative)),
            Self::TrashPath { .. } => inside(&relative),
            _ => Ok(()),
        }
    }

    pub fn run(
        &self,
        cwd: &Path,
        scope: &[PathBuf],
        limits: &Limits,
        cancel: &AtomicBool,
    ) -> Outcome {
        let started = Instant::now();
        let result = self
            .check(cwd, scope, limits)
            .and_then(|()| self.target(cwd, scope, limits))
            .and_then(|(root, relative)| {
                if cancel.load(Ordering::Acquire) {
                    return Err("Cancelled".into());
                }
                match self {
                    Self::CreateDirectory { .. } => {
                        crate::ffi::create_beneath(&root, &relative, true, cancel)
                            .map(|_| "Created directory".into())
                    }
                    Self::CreateFile { .. } => {
                        crate::ffi::create_beneath(&root, &relative, false, cancel)
                            .map(|_| "Created file".into())
                    }
                    Self::RenamePath { name, .. } => {
                        let (parent, from) = leaf(&root, &relative)?;
                        let to = CString::new(name.as_bytes()).map_err(|_| "Invalid name")?;
                        rename_exclusive(&parent, &from, &parent, &to).map_err(|error| describe(&error))?;
                        Ok(format!("Renamed to {name}"))
                    }
                    Self::MovePath { destination, .. } => {
                        let (destination_root, destination_relative) = Self::resolve(destination, cwd, scope, limits)?;
                        let destination = destination_root.join(destination_relative);
                        if destination.starts_with(root.join(&relative)) {
                            return Err("A folder cannot be moved into itself".into());
                        }
                        let (parent, name) = leaf(&root, &relative)?;
                        let target = open_directory(&destination)?;
                        rename_exclusive(&parent, &name, &target, &name).map_err(|error| describe(&error))?;
                        Ok(format!("Moved into {}", destination.display()))
                    }
                    Self::TrashPath { .. } => {
                        let trash = trash_directory().ok_or("Trash unavailable")?;
                        let (parent, name) = leaf(&root, &relative)?;
                        let bin = open_directory(&trash)?;
                        let original = relative.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                        for attempt in 1..=99 {
                            let candidate = if attempt == 1 { original.clone() } else { numbered(&original, attempt) };
                            let to = CString::new(candidate.as_bytes()).map_err(|_| "Invalid path")?;
                            match rename_exclusive(&parent, &name, &bin, &to) {
                                Ok(()) => return Ok(format!("Moved to Trash as {candidate}")),
                                Err(error) if matches!(error.raw_os_error(), Some(libc::EEXIST | libc::ENOTEMPTY)) => {}
                                Err(error) => return Err(describe(&error)),
                            }
                        }
                        Err("The Trash already holds too many items with that name".into())
                    }
                    Self::ListDirectory { .. } => {
                        let path = root
                            .join(relative)
                            .canonicalize()
                            .map_err(|_| "Directory unavailable")?;
                        if !path.starts_with(root) {
                            return Err("Path is outside the allowed directory".into());
                        }
                        let entries =
                            std::fs::read_dir(path).map_err(|_| "Directory unavailable")?;
                        let mut names = Vec::new();
                        for entry in entries.take(500) {
                            if cancel.load(Ordering::Acquire) {
                                return Err("Cancelled".into());
                            }
                            if let Ok(entry) = entry {
                                names.push(entry.file_name().to_string_lossy().into_owned());
                            }
                        }
                        names.sort();
                        Ok(names.join("\n"))
                    }
                }
            });
        Outcome {
            status: Some(if result.is_ok() { 0 } else { 1 }),
            output: result.unwrap_or_else(|e| e),
            duration: started.elapsed(),
            detached: false,
            timed_out: false,
        }
    }
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err("A new name must be a single file or folder name".into());
    }
    Ok(())
}

fn inside(relative: &Path) -> Result<(), String> {
    if relative.as_os_str().is_empty() {
        return Err("Name a file or folder inside the allowed directory, not the directory itself".into());
    }
    Ok(())
}

fn open_at(fd: i32, name: &std::ffi::CStr, flags: i32) -> Result<OwnedFd, String> {
    // SAFETY: name is NUL-terminated, fd is borrowed for the call, and no creation flag is passed.
    let raw = unsafe { libc::openat(fd, name.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if raw < 0 {
        return Err("Folder unavailable or reached through a symlink".into());
    }
    // SAFETY: a successful openat returns a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Opens `path` one component at a time without following symlinks, so a link swapped into the
/// path cannot redirect a rename, move or trash outside the allowed directory.
fn open_directory(path: &Path) -> Result<OwnedFd, String> {
    let mut fd = open_at(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY)?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let name = CString::new(name.as_bytes()).map_err(|_| "Invalid path")?;
                fd = open_at(fd.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
            }
            _ => return Err("Invalid path".into()),
        }
    }
    Ok(fd)
}

fn leaf(root: &Path, relative: &Path) -> Result<(OwnedFd, CString), String> {
    let name = relative.file_name().ok_or("Invalid path")?;
    let parent = root.join(relative.parent().unwrap_or(Path::new("")));
    Ok((open_directory(&parent)?, CString::new(name.as_bytes()).map_err(|_| "Invalid path")?))
}

/// RENAME_EXCL makes the kernel refuse instead of replacing an existing item, so no rename, move
/// or trash can overwrite anything.
fn rename_exclusive(from_dir: &OwnedFd, from: &std::ffi::CStr, to_dir: &OwnedFd, to: &std::ffi::CStr) -> std::io::Result<()> {
    // SAFETY: both descriptors are open directories owned by the caller and both names are NUL-terminated.
    let status = unsafe { libc::renameatx_np(from_dir.as_raw_fd(), from.as_ptr(), to_dir.as_raw_fd(), to.as_ptr(), libc::RENAME_EXCL) };
    if status == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

fn describe(error: &std::io::Error) -> String {
    match error.raw_os_error() {
        Some(libc::EEXIST | libc::ENOTEMPTY) => "Something with that name already exists there".into(),
        Some(libc::ENOENT) => "Not found".into(),
        Some(libc::EXDEV) => "That is on another volume; move it in Finder".into(),
        Some(libc::EACCES | libc::EPERM) => "Permission denied".into(),
        Some(libc::EINVAL) => "A folder cannot be moved into itself".into(),
        _ => "Could not complete".into(),
    }
}

/// `report.pdf` → `report 2.pdf`, the way Finder names a second item in the Trash.
fn numbered(name: &str, attempt: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} {attempt}.{extension}"),
        _ => format!("{name} {attempt}"),
    }
}

#[cfg(test)]
thread_local! {
    static TRASH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// The user's Trash. Moving there keeps the item recoverable, though Finder's Put Back does not
/// know its original folder.
fn trash_directory() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = TRASH.with(|trash| trash.borrow().clone()) {
        return Some(path);
    }
    crate::files::home_dir().map(|home| home.join(".Trash"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_creation_does_not_follow_links_or_overwrite() {
        let root = std::env::temp_dir().join(format!("blindspot-native-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("directory");
        let root = root.canonicalize().expect("canonical");
        let limits = Limits {
            roots: vec![root.clone()],
            timeout: std::time::Duration::from_secs(2),
        };
        let cancel = AtomicBool::new(false);
        let run = |intent: Intent| intent.run(&root, std::slice::from_ref(&root), &limits, &cancel);
        assert!(
            run(Intent::CreateDirectory {
                path: "notes/src".into()
            })
            .succeeded()
        );
        let file = Intent::CreateFile {
            path: "notes/keep.txt".into(),
        };
        assert!(run(file.clone()).succeeded());
        std::fs::write(root.join("notes/keep.txt"), "private").expect("write");
        assert!(!run(file).succeeded());
        assert_eq!(
            std::fs::read_to_string(root.join("notes/keep.txt")).expect("read"),
            "private"
        );
        std::os::unix::fs::symlink("notes", root.join("link")).expect("symlink");
        assert!(
            !run(Intent::CreateFile {
                path: "link/escape".into()
            })
            .succeeded()
        );
        assert!(!root.join("notes/escape").exists());
        assert!(
            !run(Intent::CreateFile {
                path: "../escape".into()
            })
            .succeeded()
        );
        assert!(
            !run(Intent::CreateDirectory {
                path: ".SSH/new".into()
            })
            .succeeded()
        );
        assert!(
            !run(Intent::CreateDirectory {
                path: "library/new".into()
            })
            .succeeded()
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn schema_rejects_unknown_operations_and_fields() {
        for input in [
            r#"{"kind":"shell","command":"id"}"#,
            r#"{"kind":"create_file","path":"x","shell":"id"}"#,
            r#"{"kind":"create_file"}"#,
        ] {
            assert!(serde_json::from_str::<Intent>(input).is_err());
        }
        for input in [
            "python3 -c evil",
            "mkdir -p x && touch y",
            "touch ../secret",
            "touch $(id)",
        ] {
            let intent = Intent::legacy(input);
            assert!(intent.is_none() || input.contains("../"));
        }
    }

    #[test]
    fn rename_move_and_trash_stay_inside_scope_and_never_overwrite() {
        let root = std::env::temp_dir().join(format!("blindspot-mutate-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).expect("directory");
        let root = root.canonicalize().expect("canonical");
        let trash = root.join("bin");
        std::fs::create_dir(&trash).expect("trash");
        TRASH.with(|slot| *slot.borrow_mut() = Some(trash.clone()));
        let limits = Limits { roots: vec![root.clone()], timeout: std::time::Duration::from_secs(2) };
        let cancel = AtomicBool::new(false);
        let run = |intent: Intent| intent.run(&root, std::slice::from_ref(&root), &limits, &cancel);
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        assert!(run(Intent::RenamePath { path: "a.txt".into(), name: "c.txt".into() }).succeeded());
        assert!(root.join("c.txt").exists() && !root.join("a.txt").exists());
        assert!(!run(Intent::RenamePath { path: "c.txt".into(), name: "b.txt".into() }).succeeded());
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b");
        assert!(!run(Intent::RenamePath { path: "c.txt".into(), name: "../escape.txt".into() }).succeeded());
        assert!(run(Intent::MovePath { path: "c.txt".into(), destination: "sub".into() }).succeeded());
        assert!(root.join("sub/c.txt").exists());
        assert!(!run(Intent::MovePath { path: "sub".into(), destination: "sub".into() }).succeeded());
        std::os::unix::fs::symlink(root.join("sub"), root.join("link")).unwrap();
        assert!(!run(Intent::MovePath { path: "b.txt".into(), destination: "link".into() }).succeeded());
        assert!(!run(Intent::MovePath { path: "b.txt".into(), destination: "../".into() }).succeeded());
        assert!(!run(Intent::TrashPath { path: ".".into() }).succeeded());
        assert!(run(Intent::TrashPath { path: "b.txt".into() }).succeeded());
        std::fs::write(root.join("b.txt"), "again").unwrap();
        assert!(run(Intent::TrashPath { path: "b.txt".into() }).succeeded());
        assert!(trash.join("b.txt").exists() && trash.join("b 2.txt").exists());
        assert!(Intent::TrashPath { path: "x".into() }.destructive());
        assert!(!Intent::ListDirectory { path: "x".into() }.destructive());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn schema_accepts_new_mutations_and_still_rejects_extra_fields() {
        assert!(serde_json::from_str::<Intent>(r#"{"kind":"rename_path","path":"a","name":"b"}"#).is_ok());
        assert!(serde_json::from_str::<Intent>(r#"{"kind":"move_path","path":"a","destination":"b"}"#).is_ok());
        assert!(serde_json::from_str::<Intent>(r#"{"kind":"trash_path","path":"a"}"#).is_ok());
        assert!(serde_json::from_str::<Intent>(r#"{"kind":"trash_path","path":"a","force":true}"#).is_err());
        assert!(serde_json::from_str::<Intent>(r#"{"kind":"delete_path","path":"a"}"#).is_err());
    }
}
