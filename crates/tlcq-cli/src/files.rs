//! Whole-file reads and writes for the state directories: JSON written to a
//! temporary name and renamed into place, so a reader never sees half of one.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::Path;

use serde::{de::DeserializeOwned, Serialize};

/// `value` as pretty JSON.
pub(crate) fn to_json<T: Serialize>(value: &T) -> std::io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(std::io::Error::other)
}

/// Read a JSON file; `None` if it does not exist.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> std::io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            std::io::Error::new(ErrorKind::InvalidData, format!("{}: {e}", path.display()))
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write a JSON file whole: to a temporary name, then renamed into place.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    write_file(&tmp, &to_json(value)?)?;
    std::fs::rename(&tmp, path)
}

/// Write `bytes` to `path` and flush them to disk.
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
