pub mod doku;
pub mod minio;
pub mod xendit;

use std::time::Duration;

const PROVIDER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PROVIDER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Payment-provider HTTP client: a stalled provider fails the request instead
/// of holding a checkout or webhook worker open indefinitely.
fn provider_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        .build()
        .expect("payment provider HTTP client")
}
