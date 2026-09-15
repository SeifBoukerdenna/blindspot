//! Descriptor-anchored reads for the opt-in content index.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path};
use std::ptr::NonNull;

// Public macOS sys/stat.h flag, absent from the current libc Rust bindings.
const SF_DATALESS: u32 = 0x40000000;

pub fn background_priority() {
    unsafe extern "C" { fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32; }
    // SAFETY: public pthread API changes only the calling worker; 0x09 is QOS_CLASS_BACKGROUND from sys/qos.h.
    let _ = unsafe { pthread_set_qos_class_self_np(0x09, 0) };
}

pub struct Directory {
    handle: NonNull<libc::DIR>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Unsupported,
}

pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub device: u64,
    pub dataless: bool,
}

impl Directory {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid());
        }
        let mut fd = open_at(libc::AT_FDCWD, c"/", libc::O_DIRECTORY)?;
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    fd = open_at(fd.as_raw_fd(), &component_name(name)?, libc::O_DIRECTORY)?
                }
                _ => return Err(invalid()),
            }
        }
        Self::from_fd(fd)
    }

    fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        // SAFETY: fd is a live directory descriptor; fdopendir takes ownership only on success.
        let handle = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        let Some(handle) = NonNull::new(handle) else {
            return Err(io::Error::last_os_error());
        };
        std::mem::forget(fd);
        Ok(Self { handle })
    }

    fn fd(&self) -> i32 {
        // SAFETY: handle is exclusively owned by this Directory and remains open until Drop.
        unsafe { libc::dirfd(self.handle.as_ptr()) }
    }

    pub fn device(&self) -> io::Result<u64> {
        // SAFETY: a zeroed stat is valid output storage for fstat.
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd remains open and metadata points to writable stat storage.
        if unsafe { libc::fstat(self.fd(), &mut metadata) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(metadata.st_dev as u64)
    }

    pub fn is_local(&self) -> io::Result<bool> {
        // SAFETY: a zeroed statfs is valid output storage for fstatfs.
        let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: fd remains live and filesystem is writable output storage.
        if unsafe { libc::fstatfs(self.fd(), &mut filesystem) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(filesystem.f_flags & libc::MNT_LOCAL as u32 != 0)
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        Self::from_fd(open_at(
            self.fd(),
            &component_name(name)?,
            libc::O_DIRECTORY,
        )?)
    }

    pub fn private_child(&self, name: &OsStr, create: bool) -> io::Result<Self> {
        let component = component_name(name)?;
        if create {
            // SAFETY: descriptor is borrowed and component is one terminated, validated filename.
            if unsafe { libc::mkdirat(self.fd(),component.as_ptr(),0o700) } != 0 {
                let error=io::Error::last_os_error();
                if error.kind()!=io::ErrorKind::AlreadyExists { return Err(error); }
            }
        }
        let child=self.child(name)?;
        // SAFETY: zeroed stat is valid fstat output storage.
        let mut metadata: libc::stat=unsafe { std::mem::zeroed() };
        // SAFETY: child owns the live descriptor and metadata is writable output storage.
        if unsafe { libc::fstat(child.fd(),&mut metadata) } != 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: geteuid takes no arguments and returns the calling process's effective user ID.
        let owner=unsafe { libc::geteuid() };
        if metadata.st_uid!=owner || metadata.st_mode & 0o077 != 0 || !child.is_local()? {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Vector cache must be private and local"));
        }
        Ok(child)
    }

    pub fn remove_file(&self, name: &OsStr) -> io::Result<()> {
        let name=component_name(name)?;
        // SAFETY: descriptor remains live and name is one terminated component. Flags=0 unlinks
        // the leaf itself, never follows a symlink and never removes directories.
        if unsafe { libc::unlinkat(self.fd(),name.as_ptr(),0) } != 0 {
            let error=io::Error::last_os_error();
            if error.kind()!=io::ErrorKind::NotFound { return Err(error); }
        }
        Ok(())
    }

    pub fn file(&self, name: &OsStr) -> io::Result<File> {
        let name = component_name(name)?;
        let before = stat_at(self.fd(), &name)?;
        if before.st_mode & libc::S_IFMT != libc::S_IFREG || before.st_flags & SF_DATALESS != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Not an available regular file",
            ));
        }
        let fd = open_at(self.fd(), &name, libc::O_NONBLOCK)?;
        let file = File::from(fd);
        use std::os::unix::fs::MetadataExt;
        let after = file.metadata()?;
        if !after.is_file() || after.dev() != before.st_dev as u64 || after.ino() != before.st_ino {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "File changed during indexing",
            ));
        }
        Ok(file)
    }

    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        loop {
            // SAFETY: __error returns this thread's errno pointer, valid for a scalar write.
            unsafe {
                *libc::__error() = 0;
            }
            // SAFETY: handle is live and uniquely borrowed; the returned entry is copied before another readdir.
            let entry = unsafe { libc::readdir(self.handle.as_ptr()) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(0) {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            // SAFETY: successful readdir returned a valid dirent until the next directory operation.
            let entry = unsafe { &*entry };
            let bytes: Vec<u8> = entry
                .d_name
                .iter()
                .take(entry.d_namlen as usize)
                .map(|c| *c as u8)
                .collect();
            let name = OsString::from_vec(bytes);
            if name == "." || name == ".." {
                continue;
            }
            let metadata = stat_at(self.fd(), &component_name(&name)?)?;
            let kind = match metadata.st_mode & libc::S_IFMT {
                libc::S_IFREG => Kind::File,
                libc::S_IFDIR => Kind::Directory,
                _ => Kind::Unsupported,
            };
            return Ok(Some(Entry {
                name,
                kind,
                device: metadata.st_dev as u64,
                dataless: metadata.st_flags & SF_DATALESS != 0,
            }));
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // SAFETY: this object owns the DIR and closes it exactly once, including its descriptor.
        unsafe {
            libc::closedir(self.handle.as_ptr());
        }
    }
}

fn component_name(name: &OsStr) -> io::Result<CString> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(invalid());
    }
    CString::new(bytes).map_err(|_| invalid())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Invalid index path component")
}

fn open_at(fd: i32, name: &std::ffi::CStr, extra: i32) -> io::Result<OwnedFd> {
    // SAFETY: name is terminated, the descriptor is borrowed, and no creation flag is passed.
    let raw = unsafe {
        libc::openat(
            fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | extra,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor that has no other owner.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn stat_at(fd: i32, name: &std::ffi::CStr) -> io::Result<libc::stat> {
    // SAFETY: a zeroed stat is valid output storage for fstatat.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: name is terminated, fd is borrowed, and metadata is writable. Symlinks are never followed.
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut metadata, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn anchored_reads_survive_directory_replacement_and_refuse_symlinks() {
        let root =
            std::env::temp_dir().join(format!("blindspot-descriptor-{}", std::process::id()));
        std::fs::create_dir(&root).expect("fixture");
        let root = root.canonicalize().expect("canonical");
        std::fs::create_dir(root.join("inside")).expect("inside");
        std::fs::create_dir(root.join("outside")).expect("outside");
        std::fs::write(root.join("inside/file.txt"), "allowed").expect("inside file");
        std::fs::write(root.join("outside/file.txt"), "private").expect("outside file");
        let directory = Directory::open(&root.join("inside")).expect("open");
        std::fs::rename(root.join("inside"), root.join("moved")).expect("rename");
        std::os::unix::fs::symlink(root.join("outside"), root.join("inside")).expect("replacement");
        let mut text = String::new();
        directory
            .file(OsStr::new("file.txt"))
            .expect("anchored")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "allowed");
        assert!(Directory::open(&root.join("inside")).is_err());
        assert!(directory.file(OsStr::new("../outside/file.txt")).is_err());
        std::os::unix::fs::symlink(root.join("outside/file.txt"), root.join("moved/link.txt"))
            .expect("file symlink");
        assert!(directory.file(OsStr::new("link.txt")).is_err());
        directory.remove_file(OsStr::new("file.txt")).expect("anchored removal");
        assert!(!root.join("moved/file.txt").exists());
        assert_eq!(std::fs::read_to_string(root.join("outside/file.txt")).unwrap(),"private");
        directory.remove_file(OsStr::new("link.txt")).expect("unlink leaf only");
        assert!(root.join("outside/file.txt").exists());
        assert!(directory.remove_file(OsStr::new("../outside/file.txt")).is_err());
        let cache=directory.private_child(OsStr::new("cache"),true).expect("private cache");
        assert!(directory.remove_file(OsStr::new("cache")).is_err());
        drop(cache);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("moved/cache"),std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(directory.private_child(OsStr::new("cache"),false).is_err());
        drop(directory);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
