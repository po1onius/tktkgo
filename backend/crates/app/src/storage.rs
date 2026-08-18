use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, instrument};

use crate::{AppError, AppResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredObject {
    pub key: String,
    pub public_url: String,
    pub byte_size: i64,
    pub checksum: String,
}

#[async_trait]
pub trait AssetStore: Send + Sync {
    async fn put(&self, key: &str, content_type: &str, bytes: &[u8]) -> AppResult<StoredObject>;
}

#[derive(Clone)]
pub struct LocalAssetStore {
    root: PathBuf,
    public_base_url: String,
}

impl LocalAssetStore {
    pub async fn new(root: PathBuf, public_base_url: String) -> AppResult<Self> {
        fs::create_dir_all(&root).await?;
        Ok(Self {
            root,
            public_base_url,
        })
    }

    fn safe_path(&self, key: &str) -> AppResult<PathBuf> {
        let relative = Path::new(key);
        if relative.is_absolute()
            || relative.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(AppError::Validation(format!("非法素材存储键: {key}")));
        }
        Ok(self.root.join(relative))
    }
}

#[async_trait]
impl AssetStore for LocalAssetStore {
    #[instrument(skip(self, bytes), fields(key, content_type, byte_size = bytes.len()))]
    async fn put(&self, key: &str, content_type: &str, bytes: &[u8]) -> AppResult<StoredObject> {
        let path = self.safe_path(key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        // 同一个内容键允许覆盖，写入结果由 generation_tasks 的幂等键保护。
        fs::write(&path, bytes).await?;
        let checksum = blake3::hash(bytes).to_hex().to_string();
        debug!(path = %path.display(), content_type, checksum, "素材已写入本地对象存储适配器");
        Ok(StoredObject {
            key: key.to_owned(),
            public_url: format!("{}/{}", self.public_base_url, key),
            byte_size: bytes.len() as i64,
            checksum,
        })
    }
}
