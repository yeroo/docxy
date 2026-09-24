//! Atomic document writes and filesystem-identity guards for exports.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempPath(PathBuf, bool);

impl Drop for TempPath {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn create_temp(
    dest: &Path,
    private: bool,
    mut next: impl FnMut() -> u64,
) -> io::Result<(TempPath, File)> {
    let parent = parent_dir(dest);
    if dest.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination has no file name",
        ));
    }
    for _ in 0..100 {
        let path = parent.join(format!(".offxy-{}-{}.tmp", std::process::id(), next()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        match options.open(&path) {
            Ok(file) => return Ok((TempPath(path, true), file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot create a unique temporary sibling",
    ))
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Write and sync a temporary sibling, then replace the destination.
/// Existing symlinks are followed (dangling links fail); existing permissions
/// are preserved. New files use the normal creation permissions (0666 filtered
/// by umask on Unix); only replacement temps start private. Existing files must
/// be writable by the caller. On Unix, owner/group are preserved: if they cannot
/// be restored on the temp, the synced bytes are copied into the original file
/// instead. That ownership-preserving fallback is not atomic and an I/O failure
/// during copying can leave partial output, matching an ordinary in-place write.
/// Otherwise failures before replacement leave the destination intact and
/// replacement changes file identity (other hard links keep the old bytes).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, |file| file.write_all(bytes))
}

/// The streaming form of [`write_atomic`]. The writer must return any write error.
pub fn write_atomic_with(
    path: &Path,
    writer: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    write_atomic_with_ownership(path, writer, restore_ownership)
}

fn write_atomic_with_ownership(
    path: &Path,
    writer: impl FnOnce(&mut File) -> io::Result<()>,
    restore: impl FnOnce(&File, &fs::Metadata) -> io::Result<bool>,
) -> io::Result<()> {
    let dest = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    // Opening without truncate verifies this caller's access, not merely the
    // presence of some write bit. Keep the handle for an ownership fallback.
    let destination = match OpenOptions::new().write(true).open(&dest) {
        Ok(file) => {
            let metadata = file.metadata()?;
            Some((file, metadata))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if destination
        .as_ref()
        .is_some_and(|(_, meta)| meta.permissions().readonly())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "destination is read-only",
        ));
    }
    // Declare the guard before the file so unwinding also closes before cleanup.
    let (mut temp, mut file) = create_temp(&dest, destination.is_some(), || {
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    })?;
    let result = (|| {
        writer(&mut file)?;
        let fallback = if let Some((destination, metadata)) = destination {
            if restore(&file, &metadata)? {
                // chown can clear permission bits, so restore mode afterward.
                file.set_permissions(metadata.permissions())?;
                None
            } else {
                Some(destination)
            }
        } else {
            None
        };
        file.sync_all()?;
        Ok::<_, io::Error>(fallback)
    })();
    drop(file);
    if let Some(mut destination) = result? {
        let mut source = File::open(&temp.0)?;
        destination.set_len(0)?;
        io::copy(&mut source, &mut destination)?;
        destination.sync_all()?;
        return Ok(());
    }
    fs::rename(&temp.0, &dest)?;
    temp.1 = false;
    #[cfg(unix)]
    if let Ok(dir) = File::open(parent_dir(&dest)) {
        // The write has committed. A directory-sync error cannot undo it.
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn restore_ownership(file: &File, original: &fs::Metadata) -> io::Result<bool> {
    use std::os::unix::fs::{MetadataExt, fchown};
    let matches =
        |meta: &fs::Metadata| meta.uid() == original.uid() && meta.gid() == original.gid();
    if matches(&file.metadata()?) {
        return Ok(true);
    }
    if fchown(file, Some(original.uid()), Some(original.gid())).is_err() {
        // An unprivileged owner may still restore a group they belong to.
        let _ = fchown(file, None, Some(original.gid()));
    }
    Ok(matches(&file.metadata()?))
}

#[cfg(not(unix))]
fn restore_ownership(_file: &File, _original: &fs::Metadata) -> io::Result<bool> {
    Ok(true)
}

/// Publish a new file, refusing any existing destination, even one created
/// during the write. Publication uses an atomic hard link to a synced sibling.
/// If linking is unavailable, falls back to exclusive creation and copying the
/// completed bytes. That fallback preserves existing files but can leave a
/// partial new destination on an I/O failure, like a normal exclusive write.
pub fn create_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    create_atomic_with(path, |file| file.write_all(bytes))
}

fn create_atomic_with(
    path: &Path,
    writer: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    create_atomic_with_link(path, writer, |source, dest| fs::hard_link(source, dest))
}

fn create_atomic_with_link(
    path: &Path,
    writer: impl FnOnce(&mut File) -> io::Result<()>,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let (temp, mut file) = create_temp(path, false, || NEXT_TEMP.fetch_add(1, Ordering::Relaxed))?;
    let result = writer(&mut file).and_then(|()| file.sync_all());
    drop(file);
    result?;
    // hard_link never replaces an existing path, unlike rename on Unix.
    if let Err(error) = link(&temp.0, path) {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Err(error);
        }
        // FAT/exFAT and some network shares cannot link. Keep the old exclusive
        // creation behavior there; a racing destination must still be refused.
        let mut source = File::open(&temp.0)?;
        let mut destination = OpenOptions::new().write(true).create_new(true).open(path)?;
        io::copy(&mut source, &mut destination)?;
        destination.sync_all()?;
    }
    drop(temp);
    #[cfg(unix)]
    if let Ok(dir) = File::open(parent_dir(path)) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Export without replacing the source, even through a symlink or hard link.
pub fn export_atomic(source: Option<&Path>, dest: &Path, bytes: &[u8]) -> io::Result<()> {
    if source.is_some_and(|source| same_file(source, dest)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot overwrite the source document",
        ));
    }
    write_atomic(dest, bytes)
}

/// Compare filesystem identity, following symlinks. Missing/inaccessible paths
/// return false. Unix uses device/inode; Windows uses volume/file index.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (identity(a), identity(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(unix)]
fn identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct FileInfo {
        attributes: u32,
        creation: [u32; 2],
        access: [u32; 2],
        write: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut FileInfo) -> i32;
    }
    // Query attributes without requiring read access; allow directory handles.
    let file = OpenOptions::new()
        .read(true)
        .access_mode(0)
        .custom_flags(0x02000000)
        .open(path)?; // FILE_FLAG_BACKUP_SEMANTICS
    let mut info = FileInfo::default();
    // SAFETY: file owns a live handle and info has the Win32 structure's layout.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        u64::from(info.volume),
        (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    ))
}

#[cfg(not(any(unix, windows)))]
fn identity(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "opccore-fsio-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> PathBuf {
            self.0.join("document")
        }
        fn count(&self) -> usize {
            fs::read_dir(&self.0).unwrap().count()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn replaces_and_cleans_up_after_writer_and_replace_errors() {
        let dir = Dir::new();
        let path = dir.file();
        write_atomic(&path, b"old").unwrap();
        let err = write_atomic_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "injected write failure");
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(dir.count(), 1);
        write_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(dir.count(), 1);
        let dest = dir.0.join("directory");
        fs::create_dir(&dest).unwrap();
        assert!(write_atomic(&dest, b"bad").is_err());
        assert!(dest.is_dir());
        assert_eq!(dir.count(), 2);
    }

    #[test]
    fn exclusive_creation_is_atomic_and_never_clobbers_a_racing_file() {
        let dir = Dir::new();
        let path = dir.file();
        let err = create_atomic_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "injected");
        assert!(!path.exists());
        assert_eq!(dir.count(), 0);
        let err = create_atomic_with(&path, |file| {
            file.write_all(b"ours")?;
            fs::write(&path, b"racing writer")
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"racing writer");
        assert_eq!(dir.count(), 1);
        assert!(create_atomic(&path, b"bad").is_err());
        fs::remove_file(&path).unwrap();
        create_atomic(&path, b"complete").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete");
        assert_eq!(dir.count(), 1);
    }

    #[test]
    fn exclusive_creation_falls_back_without_links_but_never_overwrites() {
        let dir = Dir::new();
        let path = dir.file();
        create_atomic_with_link(
            &path,
            |f| f.write_all(b"complete PDF"),
            |_, _| Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links")),
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete PDF");
        assert_eq!(dir.count(), 1);
        fs::remove_file(&path).unwrap();
        for kind in [io::ErrorKind::Unsupported, io::ErrorKind::AlreadyExists] {
            let error = create_atomic_with_link(
                &path,
                |f| f.write_all(b"ours"),
                |_, dest| {
                    fs::write(dest, b"racing writer")?;
                    Err(io::Error::new(kind, "injected link failure"))
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&path).unwrap(), b"racing writer");
            assert_eq!(dir.count(), 1);
            fs::remove_file(&path).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_files_use_default_permissions_and_replacement_temps_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new();
        let path = dir.file();
        // Do not assume a particular umask or change the process-global umask.
        let probe = dir.0.join("mode-probe");
        let default_mode = File::create(&probe)
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        fs::remove_file(probe).unwrap();
        for existing_mode in [None, Some(0o600), Some(0o644)] {
            if let Some(mode) = existing_mode {
                fs::write(&path, b"old").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            }
            write_atomic_with(&path, |file| {
                // Existing content stays private during replacement. With no
                // destination, use the same mode as ordinary file creation.
                let temporary_mode = if existing_mode.is_some() {
                    default_mode & 0o600
                } else {
                    default_mode
                };
                assert_eq!(
                    file.metadata()?.permissions().mode() & 0o777,
                    temporary_mode
                );
                file.write_all(b"private while writing")
            })
            .unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                existing_mode.unwrap_or(default_mode)
            );
            fs::remove_file(&path).unwrap();
        }
        // Both exclusive-publication paths must also keep ordinary creation modes.
        create_atomic(&path, b"linked").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            default_mode
        );
        fs::remove_file(&path).unwrap();
        create_atomic_with_link(
            &path,
            |f| f.write_all(b"fallback"),
            |_, _| Err(io::Error::new(io::ErrorKind::Unsupported, "no hard links")),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            default_mode
        );
    }

    #[cfg(unix)]
    #[test]
    fn writable_replacement_preserves_ownership_and_changes_inode() {
        use std::os::unix::fs::MetadataExt;
        let dir = Dir::new();
        let path = dir.file();
        fs::write(&path, b"old").unwrap();
        let before = fs::metadata(&path).unwrap();
        write_atomic(&path, b"new").unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        assert_ne!(after.ino(), before.ino());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(dir.count(), 1);
    }

    #[test]
    fn ownership_restore_failure_keeps_original_inode_and_cleans_temp() {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        let dir = Dir::new();
        let path = dir.file();
        fs::write(&path, b"old bytes longer than replacement").unwrap();
        let alias = dir.0.join("original-inode");
        fs::hard_link(&path, &alias).unwrap();
        #[cfg(unix)]
        let before = fs::metadata(&path).unwrap();
        write_atomic_with_ownership(&path, |file| file.write_all(b"new"), |_, _| Ok(false))
            .unwrap();
        #[cfg(unix)]
        {
            let after = fs::metadata(&path).unwrap();
            assert_eq!(
                (after.uid(), after.gid(), after.ino()),
                (before.uid(), before.gid(), before.ino())
            );
        }
        // A retained hard link identifies the original inode on both platforms,
        // so the injected fallback can also be exercised on Windows.
        assert!(same_file(&path, &alias));
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read(&alias).unwrap(), b"new");
        assert_eq!(dir.count(), 2);
        // Even when ownership would need a fallback, a failed temp write must
        // leave the original untouched and never invoke the restore hook.
        assert!(
            write_atomic_with_ownership(
                &path,
                |file| {
                    file.write_all(b"partial")?;
                    Err(io::Error::other("injected write failure"))
                },
                |_, _| panic!("must not restore after a failed write")
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(same_file(&path, &alias));
        assert_eq!(dir.count(), 2);
    }

    #[test]
    fn long_destination_name_does_not_lengthen_temp_name() {
        let dir = Dir::new();
        let name = format!("{}.docx", "x".repeat(245));
        assert_eq!(name.len(), 250);
        let path = dir.0.join(name);
        write_atomic(&path, b"first save").unwrap();
        write_atomic(&path, b"replacement").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(dir.count(), 1);
        fs::remove_file(&path).unwrap();
        create_atomic(&path, b"exclusive export").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"exclusive export");
        assert_eq!(dir.count(), 1);
    }

    #[test]
    fn readonly_destination_is_preserved() {
        let dir = Dir::new();
        let path = dir.file();
        fs::write(&path, b"old").unwrap();
        let original = fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let result = write_atomic(&path, b"bad");
        fs::set_permissions(&path, original).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(dir.count(), 1);
    }

    #[test]
    fn identities_and_export_guard() {
        let dir = Dir::new();
        let path = dir.file();
        fs::write(&path, b"source").unwrap();
        let link = dir.0.join("hardlink");
        fs::hard_link(&path, &link).unwrap();
        let other = dir.0.join("other");
        fs::write(&other, b"source").unwrap();
        fs::create_dir(dir.0.join("child")).unwrap();
        assert!(same_file(&path, &dir.0.join("./document")));
        assert!(same_file(&path, &dir.0.join("child/../document")));
        assert!(same_file(&path, &link));
        assert!(!same_file(&path, &other));
        assert!(!same_file(&path, &dir.0.join("missing")));
        assert!(!same_file(&dir.0.join("missing"), &path));
        #[cfg(windows)]
        assert!(same_file(&path, &dir.0.join("DOCUMENT")));
        assert_eq!(
            export_atomic(Some(&path), &link, b"bad")
                .unwrap_err()
                .to_string(),
            "cannot overwrite the source document"
        );
        assert_eq!(fs::read(&path).unwrap(), b"source");
        assert_eq!(fs::read(&link).unwrap(), b"source");
        export_atomic(Some(&path), &other, b"export").unwrap();
        assert_eq!(fs::read(other).unwrap(), b"export");
    }

    #[test]
    fn skips_stale_temp_collision() {
        let dir = Dir::new();
        let mut counter = 0;
        let (first, file) = create_temp(&dir.file(), false, || 0).unwrap();
        drop(file);
        let (second, file) = create_temp(&dir.file(), false, || {
            let n = counter;
            counter += 1;
            n
        })
        .unwrap();
        drop(file);
        assert_ne!(first.0, second.0);
        assert_eq!(counter, 2);
        drop((first, second));
        assert_eq!(dir.count(), 0);
    }

    #[test]
    fn bare_relative_filename_and_absolute_identity() {
        // Avoid changing cwd: independent tests can run concurrently.
        let path = PathBuf::from(format!(
            "fsio-relative-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let _cleanup = TempPath(path.clone(), true);
        write_atomic(&path, b"relative").unwrap();
        assert!(same_file(&path, &fs::canonicalize(&path).unwrap()));
        assert_eq!(fs::read(&path).unwrap(), b"relative");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = Dir::new();
        let path = dir.file();
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.0.join("symlink");
        symlink(&path, &link).unwrap();
        assert!(same_file(&path, &link));
        assert!(export_atomic(Some(&path), &link, b"bad").is_err());
        write_atomic(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_file(&path).unwrap();
        assert!(write_atomic(&link, b"bad").is_err());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(dir.count(), 1);
    }
}
