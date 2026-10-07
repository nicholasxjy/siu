//! Small JSON files kept between runs, each with the time it was written,
//! so a list can be shown at once and refreshed when it's old.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::library::now;

#[derive(Serialize, Deserialize)]
struct Stamped<T> {
    at: u64,
    data: T,
}

/// What a cache file holds.
pub struct Cached<T> {
    pub data: T,
    /// Written less than `max_age` ago.
    pub fresh: bool,
}

pub fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// The file's data, however old; None when there's none to read.
pub fn read<T: DeserializeOwned>(dir: &Path, name: &str, max_age: Duration) -> Option<Cached<T>> {
    let text = std::fs::read_to_string(path(dir, name)).ok()?;
    let s: Stamped<T> = serde_json::from_str(&text).ok()?;
    let fresh = now().saturating_sub(s.at) < max_age.as_secs();
    Some(Cached {
        data: s.data,
        fresh,
    })
}

/// Writes the data; a cache that can't be written only costs speed.
pub fn write<T: Serialize>(dir: &Path, name: &str, data: &T) {
    let Ok(text) = serde_json::to_string(&Stamped { at: now(), data }) else {
        return;
    };
    let _ = crate::mcp::write_atomic(&path(dir, name), text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_age() {
        let d = tempfile::tempdir().unwrap();
        assert!(read::<Vec<u32>>(d.path(), "x", Duration::from_secs(60)).is_none());
        write(d.path(), "x", &vec![1u32, 2]);
        let c = read::<Vec<u32>>(d.path(), "x", Duration::from_secs(60)).unwrap();
        assert_eq!((c.data, c.fresh), (vec![1, 2], true));
        assert!(
            !read::<Vec<u32>>(d.path(), "x", Duration::ZERO)
                .unwrap()
                .fresh
        );
        // a file of another shape is no cache
        std::fs::write(path(d.path(), "x"), "{\"at\":1,\"data\":\"s\"}").unwrap();
        assert!(read::<Vec<u32>>(d.path(), "x", Duration::from_secs(60)).is_none());
    }
}
