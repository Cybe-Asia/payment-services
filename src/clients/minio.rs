use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ChecksumAlgorithm;
use aws_sdk_s3::Client;
use bytes::Bytes;
use std::sync::Arc;

use crate::services::document_encryption::DocumentCipher;

#[derive(Clone)]
pub struct MinioClient {
    s3: Client,
    bucket: String,
    document_cipher: Arc<DocumentCipher>,
}

impl MinioClient {
    pub async fn new(
        endpoint: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
        bucket: &str,
        document_cipher: DocumentCipher,
    ) -> Self {
        let creds = Credentials::new(access_key, secret_key, None, None, "payment-service");
        let conf = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region.to_string()))
            .credentials_provider(creds)
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build();

        Self {
            s3: Client::from_conf(conf),
            bucket: bucket.to_string(),
            document_cipher: Arc::new(document_cipher),
        }
    }

    pub async fn put_object(
        &self,
        key: &str,
        content_type: &str,
        body: Bytes,
    ) -> Result<(), String> {
        self.s3
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .checksum_algorithm(ChecksumAlgorithm::Sha256)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| format!("minio put: {e}"))?;
        Ok(())
    }

    pub async fn put_encrypted_document(&self, key: &str, body: Bytes) -> Result<(), String> {
        let encrypted = self.document_cipher.encrypt(key, &body)?;
        self.put_object(key, "application/octet-stream", encrypted)
            .await
    }

    pub async fn get_decrypted_document(&self, key: &str) -> Result<Bytes, String> {
        let response = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| format!("minio get: {error}"))?;
        let stored = response
            .body
            .collect()
            .await
            .map_err(|error| format!("minio stream: {error}"))?
            .into_bytes();
        self.document_cipher.decrypt(key, &stored)
    }

    pub async fn delete_object(&self, key: &str) -> Result<(), String> {
        self.s3
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| format!("minio delete: {error}"))?;
        Ok(())
    }

    pub async fn migrate_legacy_documents(&self, prefix: &str) -> Result<usize, String> {
        let mut continuation_token = None;
        let mut migrated = 0;
        loop {
            let page = self
                .s3
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .set_continuation_token(continuation_token)
                .send()
                .await
                .map_err(|error| format!("minio list: {error}"))?;
            for key in page
                .contents()
                .iter()
                .filter_map(|object| object.key().map(str::to_string))
            {
                let response = self
                    .s3
                    .get_object()
                    .bucket(&self.bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(|error| format!("minio get: {error}"))?;
                let stored = response
                    .body
                    .collect()
                    .await
                    .map_err(|error| format!("minio stream: {error}"))?
                    .into_bytes();
                if DocumentCipher::is_encrypted(&stored) {
                    self.document_cipher.decrypt(&key, &stored)?;
                    continue;
                }
                let plaintext = self.document_cipher.decrypt(&key, &stored)?;
                let encrypted = self.document_cipher.encrypt(&key, &plaintext)?;
                self.document_cipher.decrypt(&key, &encrypted)?;
                self.put_object(&key, "application/octet-stream", encrypted)
                    .await?;
                migrated += 1;
            }
            if !page.is_truncated().unwrap_or(false) {
                break;
            }
            continuation_token = page.next_continuation_token().map(str::to_string);
            if continuation_token.is_none() {
                return Err("minio list continuation token is missing".to_string());
            }
        }
        Ok(migrated)
    }
}
