//! Write a file by writing another one and renaming it over the top, which is
//! what keeps an interrupted run from leaving a half file that reads as whole.
//! Both the objects and the cursor land this way.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// `path` written by `write`, or left exactly as it was. The staging file is
/// beside its destination, so the rename is within one filesystem and therefore
/// atomic; it carries the destination's name so an operator seeing one knows
/// what it was becoming.
///
/// The directory is fsynced after the rename, which is what makes the rename
/// itself durable. Without it the two renames a sync makes, the object's and
/// the cursor's, are in different directories and nothing orders them across a
/// power loss: the cursor could be durable while the object it names is not,
/// and the resumed run would then skip that object for good.
pub fn replacing<T>(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<T>,
) -> io::Result<T> {
    let staged = staging(path)?;
    if let Some(parent) = staged.parent() {
        fs::create_dir_all(parent)?;
    }
    let written = || -> io::Result<T> {
        let mut file = fs::File::create(&staged)?;
        let written = write(&mut file)?;
        file.sync_all()?;
        fs::rename(&staged, path)?;
        // The rename is a directory operation, so the file's own sync says
        // nothing about it. Costs one fsync per object written.
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(written)
    };
    match written() {
        Ok(written) => Ok(written),
        Err(error) => {
            // Left behind it would accumulate, and a reader globbing the
            // download tree must not find it either way.
            let _ = fs::remove_file(&staged);
            Err(error)
        }
    }
}

/// The staging file's own path. A destination that names no file is refused
/// here: a bare `/`, or a path ending in `..`. Defaulted, the staging file
/// would land somewhere the caller never named.
fn staging(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no file to write", path.display()),
        )
    })?;
    Ok(path.with_file_name(format!("{}.staged", name.to_string_lossy())))
}
