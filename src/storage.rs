use anyhow::Result;
use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use tokio::io::AsyncReadExt;

#[async_trait]
pub trait StorageProvider: Send + Sync {
    async fn get_object(&self, bucket: &str, key: &str, max_bytes: Option<usize>) -> Result<Vec<u8>>;
    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: Vec<u8>,
        metadata: Option<std::collections::HashMap<String, String>>,
    ) -> Result<()>;
    async fn delete_object(&self, bucket: &str, key: &str) -> Result<()>;
}

pub struct S3StorageProvider {
    client: S3Client,
}

impl S3StorageProvider {
    pub fn new(client: S3Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl StorageProvider for S3StorageProvider {
    async fn get_object(&self, bucket: &str, key: &str, max_bytes: Option<usize>) -> Result<Vec<u8>> {
        let output = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await?;

        if let Some(limit) = max_bytes {
            if let Some(content_length) = output.content_length() {
                if content_length as usize > limit {
                    return Err(anyhow::anyhow!("File size {} exceeds maximum allowed of {}", content_length, limit));
                }
            }
        }

        let mut data = Vec::new();
        if let Some(limit) = max_bytes {
            output.body.into_async_read().take(limit as u64).read_to_end(&mut data).await?;
        } else {
            output.body.into_async_read().read_to_end(&mut data).await?;
        }
        
        Ok(data)
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: Vec<u8>,
        metadata: Option<std::collections::HashMap<String, String>>,
    ) -> Result<()> {
        let mut request = self
            .client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body.into());

        if let Some(meta) = metadata {
            for (k, v) in meta {
                request = request.metadata(k, v);
            }
        }

        request.send().await?;
        Ok(())
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await?;
        Ok(())
    }
}

type MockStorageMap = std::collections::HashMap<
    String,
    (Vec<u8>, Option<std::collections::HashMap<String, String>>),
>;

pub struct MockStorageProvider {
    pub objects: std::sync::Mutex<MockStorageMap>,
}

impl Default for MockStorageProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockStorageProvider {
    pub fn new() -> Self {
        Self {
            objects: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn key(bucket: &str, key: &str) -> String {
        format!("{}/{}", bucket, key)
    }
}

#[async_trait]
impl StorageProvider for MockStorageProvider {
    async fn get_object(&self, bucket: &str, key: &str, max_bytes: Option<usize>) -> Result<Vec<u8>> {
        let key = Self::key(bucket, key);
        let data = self.objects
            .lock()
            .unwrap()
            .get(&key)
            .map(|(data, _)| data.clone())
            .ok_or_else(|| anyhow::anyhow!("Object not found: {}", key))?;
            
        if let Some(limit) = max_bytes {
            if data.len() > limit {
                return Err(anyhow::anyhow!("Mock file size {} exceeds maximum allowed of {}", data.len(), limit));
            }
        }
        
        Ok(data)
    }

    async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: Vec<u8>,
        metadata: Option<std::collections::HashMap<String, String>>,
    ) -> Result<()> {
        let key = Self::key(bucket, key);
        self.objects.lock().unwrap().insert(key, (body, metadata));
        Ok(())
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        let key = Self::key(bucket, key);
        self.objects.lock().unwrap().remove(&key);
        Ok(())
    }
}
