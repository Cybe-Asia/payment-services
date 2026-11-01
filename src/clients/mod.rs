pub mod doku;
pub mod minio;
pub mod xendit;

use std::time::Duration;

const OUTBOUND_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const OUTBOUND_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Outbound HTTP client (providers, notification-service): a stalled peer fails the
/// request instead of holding a checkout or webhook worker open indefinitely.
pub(crate) fn outbound_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(OUTBOUND_CONNECT_TIMEOUT)
        .timeout(OUTBOUND_REQUEST_TIMEOUT)
        .build()
        .expect("outbound HTTP client")
}
