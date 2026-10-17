//! Blob storage for operation payloads and snapshots: files under `CALC_BLOB_DIR`, named by content hash.
//!
//! Blobs are shared by content: two sheets holding the same bytes hold one file. So a blob is only removed when no
//! row points at it any more, and never while someone may be about to point at it: storing bytes that are already
//! there marks the file as just used, and removal leaves recently used files alone (see `remove_unused`).

use std::{future::Future, path::PathBuf, time::Duration};

use sha2::{Digest as _, Sha256};

use crate::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct Blobs {
    root: PathBuf,
}

/// What `remove_unused` did with one blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// Some row still points at it.
    InUse,
    /// It was stored or reused less than the grace period ago; try again later.
    Recent,
    /// There was no such file.
    Missing,
}

impl Blobs {
    pub fn new(root: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path(&self, key: &str) -> ApiResult<PathBuf> {
        if key.len() < 8 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ApiError::internal(format!("bad blob key {key}")));
        }
        Ok(self.root.join(&key[..2]).join(key))
    }

    /// Stores bytes and returns their key (the SHA-256, hex). Storing the same bytes twice is one file; storing bytes
    /// that are already there marks the file as just used.
    pub async fn put(&self, bytes: &[u8]) -> ApiResult<String> {
        let key = hex::encode(Sha256::digest(bytes));
        let path = self.path(&key)?;
        if touch(path.clone()).await {
            return Ok(key);
        }
        let dir = path.parent().expect("has a parent").to_owned();
        tokio::fs::create_dir_all(&dir).await.map_err(ApiError::internal)?;
        let tmp = dir.join(format!(".{key}.{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&tmp, bytes).await.map_err(ApiError::internal)?;
        tokio::fs::rename(&tmp, &path).await.map_err(ApiError::internal)?;
        Ok(key)
    }

    pub async fn get(&self, key: &str) -> ApiResult<Vec<u8>> {
        let path = self.path(key)?;
        tokio::fs::read(&path).await.map_err(|e| ApiError::internal(format!("reading blob {key}: {e}")))
    }

    /// Whether a blob is stored.
    pub async fn exists(&self, key: &str) -> bool {
        match self.path(key) {
            Ok(p) => tokio::fs::try_exists(&p).await.unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Removes a blob nothing points at. `in_use` says whether some row points at the key now; it is asked twice,
    /// before and after the file is moved aside, and a file stored or reused within `grace` stays (whoever stored
    /// it is about to point at it). Safe against `put` running at the same moment: a `put` after the move writes a
    /// fresh file, and a `put` before it shows up as a recent use.
    pub async fn remove_unused<F, Fut>(&self, key: &str, grace: Duration, in_use: F) -> ApiResult<Removal>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = ApiResult<bool>>,
    {
        let path = self.path(key)?;
        if in_use().await? {
            return Ok(Removal::InUse);
        }
        match used_within(&path, grace).await {
            None => return Ok(Removal::Missing),
            Some(true) => return Ok(Removal::Recent),
            Some(false) => {}
        }
        let aside = path.with_file_name(format!(".gc.{key}.{}", uuid::Uuid::new_v4()));
        if tokio::fs::rename(&path, &aside).await.is_err() {
            return Ok(Removal::Missing);
        }
        let recent = used_within(&aside, grace).await.unwrap_or(true);
        let wanted = recent || in_use().await.unwrap_or(true);
        if wanted {
            // Put it back, unless a fresh copy of the same bytes was stored meanwhile.
            if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                let _ = tokio::fs::remove_file(&aside).await;
            } else {
                tokio::fs::rename(&aside, &path).await.map_err(ApiError::internal)?;
            }
            return Ok(if recent { Removal::Recent } else { Removal::InUse });
        }
        tokio::fs::remove_file(&aside).await.map_err(ApiError::internal)?;
        Ok(Removal::Removed)
    }
}

/// Marks an existing file as just used. False when there is no such file.
async fn touch(path: PathBuf) -> bool {
    tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()))
            .is_ok()
    })
    .await
    .unwrap_or(false)
}

/// Whether a file was stored or reused within `grace` (None: no such file).
async fn used_within(path: &std::path::Path, grace: Duration) -> Option<bool> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let modified = meta.modified().ok()?;
    Some(modified.elapsed().map(|age| age < grace).unwrap_or(true))
}
