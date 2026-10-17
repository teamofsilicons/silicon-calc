//! Blob storage for operation payloads and snapshots: files under `CALC_BLOB_DIR`, named by content hash.

use std::path::PathBuf;

use sha2::{Digest as _, Sha256};

use crate::error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct Blobs {
    root: PathBuf,
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

    /// Stores bytes and returns their key (the SHA-256, hex). Storing the same bytes twice is one file.
    pub async fn put(&self, bytes: &[u8]) -> ApiResult<String> {
        let key = hex::encode(Sha256::digest(bytes));
        let path = self.path(&key)?;
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
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
}
