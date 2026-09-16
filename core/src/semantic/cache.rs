//! Private derived-vector files. Callers serialize publication and pruning on the index worker.

use crate::{
    content::{
        Error, Result,
        vectors::{MAX_SHARDS, Shard},
    },
    ffi::index_native::Directory,
};
use std::{
    collections::HashSet,
    ffi::OsStr,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const DIRECTORY: &str = "content-vectors";

pub fn prepare(database: &Path) -> Result<PathBuf> {
    let parent = parent(database)?;
    let directory = Directory::open(&parent)?;
    directory.private_child(OsStr::new(DIRECTORY), true)?;
    Ok(parent.join(DIRECTORY))
}

pub fn token() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn available(database: &Path, shard: &Shard) -> Result<bool> {
    shard.validate()?;
    let Some(directory) = open(database)? else {
        return Ok(false);
    };
    let file = match directory.file(OsStr::new(&format!("{}.ann", shard.token))) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    Ok(metadata.len() == shard.bytes && metadata.nlink() == 1 && metadata.mode() & 0o077 == 0)
}

pub fn prune(database: &Path, retained: &[Shard], cancel: &AtomicBool) -> Result<u64> {
    check(cancel)?;
    if retained.len() > MAX_SHARDS {
        return Err(Error::Invalid("Vector cache capacity exceeded"));
    }
    for shard in retained {
        shard.validate()?;
    }
    let keep: HashSet<_> = retained.iter().map(|shard| shard.token.as_str()).collect();
    let Some(mut directory) = open(database)? else {
        return Ok(0);
    };
    let mut removed = 0;
    let mut examined = 0;
    loop {
        check(cancel)?;
        let Some(entry) = directory.next_entry()? else {
            return Ok(removed);
        };
        examined += 1;
        if examined > 10_000 {
            return Err(Error::Invalid("Vector cache cleanup incomplete; retry"));
        }
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let Some(token) = name.strip_suffix(".ann") else {
            continue;
        };
        if token.len() != 32
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            continue;
        }
        if keep.contains(token) {
            continue;
        }
        directory.remove_file(&entry.name)?;
        removed += 1;
    }
}

fn parent(database: &Path) -> Result<PathBuf> {
    if !database.is_absolute()
        || database
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(Error::Invalid("Invalid content database path"));
    }
    let parent = database
        .parent()
        .ok_or(Error::Invalid("Invalid content database path"))?;
    Ok(parent.canonicalize()?)
}

fn open(database: &Path) -> Result<Option<Directory>> {
    let parent = match parent(database) {
        Ok(parent) => parent,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match Directory::open(&parent)?.private_child(OsStr::new(DIRECTORY), false) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn cleanup_keeps_published_artifacts_unlinks_orphan_links_and_preserves_unrelated_files() {
        let root =
            std::env::temp_dir().join(format!("blindspot-vector-cache-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("content.sqlite");
        assert_eq!(prune(&database, &[], &AtomicBool::new(false)).unwrap(), 0);
        assert!(!root.join(DIRECTORY).exists());
        let cache = prepare(&database).unwrap();
        let shard = Shard {
            after: 0,
            through: 1,
            model: "fixture".into(),
            dimensions: 2,
            token: token().unwrap(),
            checksum: "b".repeat(64),
            count: 1,
            bytes: 64,
        };
        let published = cache.join(format!("{}.ann", shard.token));
        std::fs::write(&published, [0; 64]).unwrap();
        std::fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(available(&database, &shard).unwrap());
        let orphan = cache.join(format!("{}.ann", token().unwrap()));
        std::fs::write(&orphan, b"partial native write").unwrap();
        let outside = root.join("source.txt");
        std::fs::write(&outside, b"source").unwrap();
        let link = cache.join(format!("{}.ann", token().unwrap()));
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        std::fs::write(cache.join("notes.txt"), b"unrelated").unwrap();
        assert!(matches!(
            prune(&database, &[], &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
        assert!(orphan.exists());
        assert_eq!(
            prune(
                &database,
                std::slice::from_ref(&shard),
                &AtomicBool::new(false)
            )
            .unwrap(),
            2
        );
        assert!(published.exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"source");
        assert_eq!(prune(&database, &[], &AtomicBool::new(false)).unwrap(), 1);
        assert!(cache.join("notes.txt").exists());
        std::fs::remove_dir_all(&cache).unwrap();
        std::os::unix::fs::symlink(&root, &cache).unwrap();
        assert!(prepare(&database).is_err());
        assert!(prune(&database, &[], &AtomicBool::new(false)).is_err());
        assert!(outside.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
