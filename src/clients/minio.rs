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

    async fn put_object(&self, key: &str, content_type: &str, body: Bytes) -> Result<(), String> {
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
            .map_err(|_| "payment evidence read failed".to_string())?;
        if response.content_length().unwrap_or(i64::MAX) > 10 * 1024 * 1024 + 128 {
            return Err("stored payment evidence exceeds size limit".into());
        }
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

    /// Audit only this service's namespace; migration never enables plaintext HTTP reads.
    pub async fn audit_documents(&self, migrate: bool) -> Result<(usize, usize), String> {
        if migrate {
            let versioning = self
                .s3
                .get_bucket_versioning()
                .bucket(&self.bucket)
                .send()
                .await
                .map_err(|_| "payment evidence versioning check failed")?;
            if versioning.status().is_some() {
                return Err(
                    "versioned bucket requires a separately planned historical-version migration"
                        .into(),
                );
            }
        }
        let mut continuation_token = None;
        let mut verified = 0;
        let mut migrated = 0;
        loop {
            let page = self
                .s3
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix("school-test/payments/")
                .set_continuation_token(continuation_token)
                .send()
                .await
                .map_err(|_| "payment evidence listing failed")?;
            for key in page.contents().iter().filter_map(|object| object.key()) {
                let response = self
                    .s3
                    .get_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(|_| "payment evidence audit read failed")?;
                if response.content_length().unwrap_or(i64::MAX) > 10 * 1024 * 1024 + 128 {
                    return Err("stored payment evidence exceeds size limit".into());
                }
                let etag = response
                    .e_tag()
                    .ok_or("payment evidence ETag missing")?
                    .to_string();
                let stored = response
                    .body
                    .collect()
                    .await
                    .map_err(|_| "payment evidence audit read failed")?
                    .into_bytes();
                if DocumentCipher::is_encrypted(&stored) {
                    self.document_cipher.decrypt(key, &stored)?;
                    verified += 1;
                    continue;
                }
                if !migrate {
                    return Err("payment evidence audit found plaintext".into());
                }
                let encrypted = self.document_cipher.encrypt(key, &stored)?;
                if self.document_cipher.decrypt(key, &encrypted)? != stored {
                    return Err("payment evidence pre-write verification failed".into());
                }
                self.s3
                    .put_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .if_match(etag)
                    .content_type("application/octet-stream")
                    .checksum_algorithm(ChecksumAlgorithm::Sha256)
                    .body(ByteStream::from(encrypted))
                    .send()
                    .await
                    .map_err(|_| "payment evidence conditional migration write failed")?;
                if self.get_decrypted_document(key).await? != stored {
                    return Err("payment evidence post-write verification failed".into());
                }
                verified += 1;
                migrated += 1;
            }
            if !page.is_truncated().unwrap_or(false) {
                break;
            }
            continuation_token = page.next_continuation_token().map(str::to_string);
            if continuation_token.is_none() {
                return Err("payment evidence continuation token missing".into());
            }
        }
        Ok((verified, migrated))
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires disposable DOCUMENT_TEST_S3_ENDPOINT"]
    async fn strict_migration_is_resumable_and_rejects_versions_and_stale_writes() {
        let endpoint = std::env::var("DOCUMENT_TEST_S3_ENDPOINT").unwrap();
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        let bucket = format!("payment-test-{}", uuid::Uuid::new_v4());
        let cipher =
            DocumentCipher::from_hex_keyring("test", &format!("test={}", "a".repeat(64)), false)
                .unwrap();
        let client = MinioClient::new(
            &endpoint,
            "us-east-1",
            "synthetic-test-user",
            "synthetic-test-password",
            &bucket,
            cipher,
        )
        .await;
        client
            .s3
            .create_bucket()
            .bucket(&bucket)
            .send()
            .await
            .unwrap();
        let key = "school-test/payments/fixture/proof";
        let untouched = "school-test/student/fixture/document";
        let plain = Bytes::from_static(b"synthetic payment evidence");
        client
            .put_object(key, "image/png", plain.clone())
            .await
            .unwrap();
        client
            .put_object(untouched, "image/png", plain.clone())
            .await
            .unwrap();
        assert!(client.get_decrypted_document(key).await.is_err());
        assert!(client.audit_documents(false).await.is_err());
        assert_eq!(client.audit_documents(true).await.unwrap(), (1, 1));
        assert_eq!(client.audit_documents(true).await.unwrap(), (1, 0));
        assert_eq!(client.audit_documents(false).await.unwrap(), (1, 0));
        assert_eq!(client.get_decrypted_document(key).await.unwrap(), plain);
        let raw = client
            .s3
            .get_object()
            .bucket(&bucket)
            .key(untouched)
            .send()
            .await
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes();
        assert_eq!(raw, plain);
        assert!(client
            .s3
            .put_object()
            .bucket(&bucket)
            .key(key)
            .if_match("\"stale\"")
            .body(ByteStream::from(plain.clone()))
            .send()
            .await
            .is_err());
        assert_eq!(client.get_decrypted_document(key).await.unwrap(), plain);
        let mut tampered = client
            .s3
            .get_object()
            .bucket(&bucket)
            .key(key)
            .send()
            .await
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .into_bytes()
            .to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        client
            .put_object(key, "application/octet-stream", Bytes::from(tampered))
            .await
            .unwrap();
        assert!(client.audit_documents(true).await.is_err());
        for object in [key, untouched] {
            client.delete_object(object).await.unwrap();
        }
        for status in [
            aws_sdk_s3::types::BucketVersioningStatus::Enabled,
            aws_sdk_s3::types::BucketVersioningStatus::Suspended,
        ] {
            client
                .s3
                .put_bucket_versioning()
                .bucket(&bucket)
                .versioning_configuration(
                    aws_sdk_s3::types::VersioningConfiguration::builder()
                        .status(status)
                        .build(),
                )
                .send()
                .await
                .unwrap();
            assert!(client
                .audit_documents(true)
                .await
                .unwrap_err()
                .contains("versioned bucket"));
        }
        client
            .s3
            .delete_bucket()
            .bucket(&bucket)
            .send()
            .await
            .unwrap();
    }
}
