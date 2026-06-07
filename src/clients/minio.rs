use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ChecksumAlgorithm;
use aws_sdk_s3::Client;
use bytes::Bytes;

#[derive(Clone)]
pub struct MinioClient {
    s3: Client,
    bucket: String,
}

impl MinioClient {
    pub async fn new(
        endpoint: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
        bucket: &str,
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

    pub async fn presigned_get(&self, key: &str, ttl_secs: u64) -> Result<String, String> {
        use aws_sdk_s3::presigning::PresigningConfig;

        let cfg = PresigningConfig::expires_in(std::time::Duration::from_secs(ttl_secs))
            .map_err(|e| format!("presigning config: {e}"))?;
        let req = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(cfg)
            .await
            .map_err(|e| format!("minio presign: {e}"))?;
        Ok(req.uri().to_string())
    }
}
