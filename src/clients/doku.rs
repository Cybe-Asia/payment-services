use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const CHECKOUT_TARGET: &str = "/checkout/v1/payment";
pub const STATUS_TARGET_PREFIX: &str = "/orders/v1/status/";

#[derive(Clone)]
pub struct DokuClient {
    http: Client,
    api_url: String,
    client_id: String,
    secret_key: String,
    return_url: String,
    notification_url: String,
    payment_method_types: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CreateCheckoutRequest<'a> {
    pub request_id: &'a str,
    pub invoice_number: &'a str,
    pub amount: i64,
    pub currency: &'a str,
    pub due_minutes: i64,
}

#[derive(Debug, Clone)]
pub struct CreateCheckoutResponse {
    pub checkout_url: String,
    pub token_id: String,
    pub session_id: Option<String>,
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
struct CheckoutEnvelope {
    response: CheckoutResponse,
}

#[derive(Debug, Deserialize)]
struct CheckoutResponse {
    order: CheckoutOrder,
    payment: CheckoutPayment,
}

#[derive(Debug, Deserialize)]
struct CheckoutOrder {
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CheckoutPayment {
    token_id: String,
    url: String,
}

impl DokuClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_url: &str,
        client_id: &str,
        secret_key: &str,
        return_url: &str,
        notification_url: &str,
        payment_method_types: Vec<String>,
    ) -> Self {
        Self {
            http: Client::new(),
            api_url: api_url.trim_end_matches('/').to_string(),
            client_id: client_id.to_string(),
            secret_key: secret_key.to_string(),
            return_url: return_url.to_string(),
            notification_url: notification_url.to_string(),
            payment_method_types,
        }
    }

    pub fn configured(&self) -> bool {
        !self.client_id.is_empty()
            && !self.secret_key.is_empty()
            && !self.return_url.is_empty()
            && !self.notification_url.is_empty()
            && !self.payment_method_types.is_empty()
    }

    pub async fn create_checkout(
        &self,
        request: &CreateCheckoutRequest<'_>,
    ) -> Result<CreateCheckoutResponse, String> {
        if !self.configured() {
            return Err("DOKU sandbox prerequisites are not configured".into());
        }
        if request.amount <= 0 || request.currency != "IDR" {
            return Err("DOKU checkout requires a positive IDR amount".into());
        }
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let body = serde_json::json!({
            "order": {
                "amount": request.amount,
                "invoice_number": request.invoice_number,
                "currency": request.currency,
                "callback_url": self.return_url,
                "callback_url_result": self.return_url,
                "auto_redirect": true,
                "disable_retry_payment": true
            },
            "payment": {
                "payment_due_date": request.due_minutes,
                "payment_method_types": self.payment_method_types
            },
            "additional_info": {
                "override_notification_url": self.notification_url
            }
        });
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| format!("DOKU request encode failed: {e}"))?;
        let signature = sign_non_snap_request(
            &self.client_id,
            request.request_id,
            &timestamp,
            CHECKOUT_TARGET,
            &body_bytes,
            &self.secret_key,
        )?;
        let response = self
            .http
            .post(format!("{}{}", self.api_url, CHECKOUT_TARGET))
            .header("Client-Id", &self.client_id)
            .header("Request-Id", request.request_id)
            .header("Request-Timestamp", &timestamp)
            .header("Signature", signature)
            .header("Content-Type", "application/json")
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| format!("DOKU checkout request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "DOKU checkout rejected request with status {status}"
            ));
        }
        let parsed = response
            .json::<CheckoutEnvelope>()
            .await
            .map_err(|e| format!("DOKU checkout response was invalid: {e}"))?;
        Ok(CreateCheckoutResponse {
            checkout_url: parsed.response.payment.url,
            token_id: parsed.response.payment.token_id,
            session_id: parsed.response.order.session_id,
            request_id: request.request_id.to_string(),
        })
    }

    /// Fetch the authoritative provider status for an existing Checkout order.
    /// DOKU's non-SNAP status endpoint is a signed GET and therefore omits the
    /// request-body Digest component.
    pub async fn check_status(
        &self,
        invoice_number: &str,
        request_id: &str,
    ) -> Result<DokuWebhook, String> {
        if !self.configured() {
            return Err("DOKU sandbox prerequisites are not configured".into());
        }
        if invoice_number.is_empty()
            || invoice_number.len() > 128
            || !invoice_number
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'-' || value == b'_')
        {
            return Err("DOKU invoice reference is invalid".into());
        }
        let target = format!("{STATUS_TARGET_PREFIX}{invoice_number}");
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let signature = sign_non_snap_get(
            &self.client_id,
            request_id,
            &timestamp,
            &target,
            &self.secret_key,
        )?;
        let response = self
            .http
            .get(format!("{}{}", self.api_url, target))
            .header("Client-Id", &self.client_id)
            .header("Request-Id", request_id)
            .header("Request-Timestamp", &timestamp)
            .header("Signature", signature)
            .send()
            .await
            .map_err(|e| format!("DOKU status request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "DOKU status request was rejected with status {status}"
            ));
        }
        response
            .json::<DokuWebhook>()
            .await
            .map_err(|e| format!("DOKU status response was invalid: {e}"))
    }
}

pub fn sign_non_snap_request(
    client_id: &str,
    request_id: &str,
    request_timestamp: &str,
    request_target: &str,
    body: &[u8],
    secret: &str,
) -> Result<String, String> {
    let digest = B64.encode(Sha256::digest(body));
    let component = format!(
        "Client-Id:{client_id}\nRequest-Id:{request_id}\nRequest-Timestamp:{request_timestamp}\nRequest-Target:{request_target}\nDigest:{digest}"
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "DOKU secret key is invalid".to_string())?;
    mac.update(component.as_bytes());
    Ok(format!(
        "HMACSHA256={}",
        B64.encode(mac.finalize().into_bytes())
    ))
}

pub fn sign_non_snap_get(
    client_id: &str,
    request_id: &str,
    request_timestamp: &str,
    request_target: &str,
    secret: &str,
) -> Result<String, String> {
    let component = format!(
        "Client-Id:{client_id}\nRequest-Id:{request_id}\nRequest-Timestamp:{request_timestamp}\nRequest-Target:{request_target}"
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "DOKU secret key is invalid".to_string())?;
    mac.update(component.as_bytes());
    Ok(format!(
        "HMACSHA256={}",
        B64.encode(mac.finalize().into_bytes())
    ))
}

pub fn verify_non_snap_webhook(
    signature: &str,
    client_id: &str,
    request_id: &str,
    request_timestamp: &str,
    request_target: &str,
    body: &[u8],
    secret: &str,
) -> bool {
    let Some(encoded) = signature.strip_prefix("HMACSHA256=") else {
        return false;
    };
    let Ok(provided) = B64.decode(encoded) else {
        return false;
    };
    let digest = B64.encode(Sha256::digest(body));
    let component = format!(
        "Client-Id:{client_id}\nRequest-Id:{request_id}\nRequest-Timestamp:{request_timestamp}\nRequest-Target:{request_target}\nDigest:{digest}"
    );
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(component.as_bytes());
    mac.verify_slice(&provided).is_ok()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DokuWebhook {
    pub order: DokuWebhookOrder,
    #[serde(default)]
    pub transaction: Option<DokuWebhookTransaction>,
    #[serde(default)]
    pub service: Option<DokuWebhookId>,
    #[serde(default)]
    pub channel: Option<DokuWebhookId>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DokuWebhookOrder {
    pub invoice_number: String,
    pub amount: serde_json::Number,
    pub currency: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DokuWebhookTransaction {
    pub status: String,
    #[serde(default)]
    pub original_request_id: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DokuWebhookId {
    #[serde(default)]
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_verification_rejects_tampering() {
        let body = br#"{"order":{"amount":1000}}"#;
        let signature = sign_non_snap_request(
            "MCH-1",
            "request-1",
            "2026-08-11T00:00:00Z",
            "/api/v1/payments/webhook/doku",
            body,
            "synthetic-secret",
        )
        .unwrap();
        assert!(verify_non_snap_webhook(
            &signature,
            "MCH-1",
            "request-1",
            "2026-08-11T00:00:00Z",
            "/api/v1/payments/webhook/doku",
            body,
            "synthetic-secret"
        ));
        assert!(!verify_non_snap_webhook(
            &signature,
            "MCH-1",
            "request-1",
            "2026-08-11T00:00:00Z",
            "/api/v1/payments/webhook/doku",
            br#"{"order":{"amount":1001}}"#,
            "synthetic-secret"
        ));
    }

    #[test]
    fn get_signature_uses_no_digest_component() {
        let signature = sign_non_snap_get(
            "MCH-1",
            "request-1",
            "2026-08-11T00:00:00Z",
            "/orders/v1/status/INV-1",
            "synthetic-secret",
        )
        .unwrap();
        assert_eq!(
            signature,
            "HMACSHA256=2WFMvqgw0KYQ8ptzflzITgIPApyqr7PvdNYoQ8ETC3I="
        );
    }
}
