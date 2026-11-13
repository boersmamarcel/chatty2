//! File writes that never follow a symlink at the target path (AGE-534,
//! AGE-1113).
//!
//! The callers check that the target's parent directory resolves inside the
//! workspace, then join the file name. `std::fs::write`, `tokio::fs::write`
//! and `OpenOptions::create(true)` would still follow a symlink planted at
//! that name (by an earlier tool call, a cloned repo or an extracted
//! archive), writing wherever it points. These helpers refuse that:
//!
//! - [`write_no_follow`]: `symlink_metadata` on the target refuses a symlink
//!   (dangling or not) or any other non-regular file; a new file is created
//!   with `create_new`, which fails on any existing entry, a symlink
//!   included (a writer that loses that race replaces the winner's file
//!   instead, AGE-1345); an existing regular file is replaced by renaming a fresh
//!   `create_new` sibling over it, which swaps the directory entry rather
//!   than writing through it.
//! - [`append_no_follow`]: the same pre-check, then on unix an open with
//!   `O_NOFOLLOW`, so a link planted between the check and the open is
//!   refused too. On Windows only the pre-check guards it: symlinks there
//!   are rare and need a privilege to create.
//! - [`entry_exists`]: what an approval card's `is_overwrite` shows, from
//!   `symlink_metadata`, so a link is not reported as its target.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// The error for a target that is a symlink, directory or other non-regular
/// file. Path-free, so callers can wrap it in their own message.
fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "not a regular file (a symlink?); refusing to write through it",
    )
}

/// Whether anything (a symlink included, dangling or not) is at `path`,
/// without following a symlink there.
pub(crate) fn entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// `target`'s metadata if it is an existing regular file, `None` if nothing
/// is there, or an error for anything we refuse to write through.
fn regular_file_or_absent(target: &Path) -> io::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(target) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(meta) if !meta.file_type().is_file() => Err(not_regular()),
        Ok(meta) => Ok(Some(meta)),
    }
}

/// The replaced file's permissions for its replacement: on unix only the
/// rwx bits, so setuid, setgid and sticky never carry over to new content.
fn replacement_permissions(meta: &std::fs::Metadata) -> std::fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(meta.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        meta.permissions()
    }
}

/// Write `bytes` to `target` without following a symlink there. Returns
/// whether an existing regular file was replaced. The replaced file's rwx
/// permissions are kept (an executable script stays executable); setuid,
/// setgid and sticky bits are not.
pub(crate) fn write_no_follow(target: &Path, bytes: &[u8]) -> io::Result<bool> {
    let create_new = |at: &Path| -> io::Result<std::fs::File> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(at)?;
        file.write_all(bytes)?;
        Ok(file)
    };

    let meta = match regular_file_or_absent(target)? {
        Some(meta) => meta,
        None => match create_new(target) {
            Ok(_) => return Ok(false),
            // Another writer created the target after the check above
            // (AGE-1345). Replace its file like any existing one; the same
            // check refuses a symlink that took the name.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                regular_file_or_absent(target)?.ok_or(e)?
            }
            Err(e) => return Err(e),
        },
    };

    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // The counter keeps concurrent writers in one process apart when the
    // clock reads the same nanosecond for both.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        ".{}.chatty-{}-{nanos}-{seq}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    // The mode is set on the open handle (fchmod), never through the tmp
    // path, so a link swapped in at that name is not chmodded.
    create_new(&tmp)
        .and_then(|file| file.set_permissions(replacement_permissions(&meta)))
        .and_then(|()| std::fs::rename(&tmp, target))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    Ok(true)
}

/// Append `bytes` to `target`, creating it if absent, without following a
/// symlink there. Returns whether the file already existed.
pub(crate) fn append_no_follow(target: &Path, bytes: &[u8]) -> io::Result<bool> {
    let existed = regular_file_or_absent(target)?.is_some();

    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut file = options.open(target).map_err(|e| {
        // ELOOP: a symlink appeared at the target after the pre-check.
        #[cfg(unix)]
        if e.raw_os_error() == Some(nix::libc::ELOOP) {
            return not_regular();
        }
        e
    })?;
    // A FIFO, device or directory swapped in after the pre-check.
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    file.write_all(bytes)?;
    file.flush()?;
    Ok(existed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_creates_then_replaces_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.txt");
        assert!(!write_no_follow(&target, b"one").unwrap());
        assert!(write_no_follow(&target, b"two").unwrap());
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_keeps_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("run.sh");
        std::fs::write(&target, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_no_follow(&target, b"#!/bin/sh\necho hi\n").unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o755);
    }

    /// Only the rwx bits carry over: setuid, setgid and sticky are dropped.
    #[cfg(unix)]
    #[test]
    fn overwrite_drops_setuid_setgid_and_sticky_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("tool");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o4755)).unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            0o4755,
            "setup: setuid set"
        );
        write_no_follow(&target, b"new").unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o755);
    }

    #[test]
    fn append_creates_then_appends() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("log.txt");
        assert!(!append_no_follow(&target, b"one\n").unwrap());
        assert!(append_no_follow(&target, b"two\n").unwrap());
        assert_eq!(std::fs::read(&target).unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn a_directory_at_the_target_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sub");
        std::fs::create_dir(&target).unwrap();
        assert!(write_no_follow(&target, b"x").is_err());
        assert!(append_no_follow(&target, b"x").is_err());
    }

    /// Writers that all see the target absent race on `create_new`; the
    /// losers must replace the winner's file, not fail with "File exists"
    /// (AGE-1345). The last write wins and no temp file is left behind.
    #[test]
    fn concurrent_writers_to_a_new_file_all_succeed() {
        const WRITERS: usize = 8;
        for round in 0..200 {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("answer.txt");
            let barrier = std::sync::Barrier::new(WRITERS);
            let results: Vec<io::Result<bool>> = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..WRITERS)
                    .map(|i| {
                        let (target, barrier) = (&target, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            write_no_follow(target, format!("writer {i}").as_bytes())
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            for result in &results {
                assert!(result.is_ok(), "round {round}: {result:?}");
            }
            let content = std::fs::read_to_string(&target).unwrap();
            assert!(
                (0..WRITERS).any(|i| content == format!("writer {i}")),
                "round {round}: one writer's content, whole: {content:?}"
            );
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_at_the_target_are_refused_and_nothing_is_written_through_them() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let dangling = dir.path().join("dangling.txt");
        let would_be = outside.path().join("created.txt");
        std::os::unix::fs::symlink(&would_be, &dangling).unwrap();

        for target in [&link, &dangling] {
            let err = write_no_follow(target, b"pwned").unwrap_err();
            assert!(err.to_string().contains("not a regular file"), "{err}");
            let err = append_no_follow(target, b"pwned").unwrap_err();
            assert!(err.to_string().contains("not a regular file"), "{err}");
            assert!(entry_exists(target));
        }
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!would_be.exists());
        // The links themselves are untouched.
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
