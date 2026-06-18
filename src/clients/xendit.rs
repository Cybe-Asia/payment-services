use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Clone)]
pub struct XenditClient {
    http: Client,
    api_url: String,
    api_key: String,
    success_redirect_url: String,
    failure_redirect_url: String,
}

impl XenditClient {
    pub fn new(
        api_url: &str,
        api_key: &str,
        success_redirect_url: &str,
        failure_redirect_url: &str,
    ) -> Self {
        Self {
            http: Client::new(),
            api_url: api_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            success_redirect_url: success_redirect_url.to_string(),
            failure_redirect_url: failure_redirect_url.to_string(),
        }
    }

    /// Create a Xendit Invoice (hosted checkout).
    /// Docs: https://api.xendit.co/v2/invoices
    pub async fn create_invoice(
        &self,
        req: &CreateInvoiceRequest<'_>,
    ) -> Result<CreateInvoiceResponse, String> {
        if self.api_key.is_empty() {
            return Err("XENDIT_API_KEY not configured".into());
        }
        let auth = format!("Basic {}", B64.encode(format!("{}:", self.api_key)));
        let body = serde_json::json!({
            "external_id": req.external_id,
            "amount": req.amount,
            "currency": req.currency,
            "description": req.description,
            "payer_email": req.payer_email,
            "customer": {
                "given_names": req.customer_name,
                "email": req.payer_email,
                "mobile_number": req.customer_phone,
            },
            "success_redirect_url": format!("{}&paymentId={}", self.success_redirect_url, req.external_id),
            "failure_redirect_url": format!("{}&paymentId={}", self.failure_redirect_url, req.external_id),
            "invoice_duration": req.invoice_duration_seconds,
        });
        let res = self
            .http
            .post(format!("{}/v2/invoices", self.api_url))
            .header("Authorization", auth)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("xendit request failed: {e}"))?;

        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            warn!(%status, "xendit invoice error body={}", text);
            return Err(format!("xendit invoice error {status}: {text}"));
        }
        let parsed: CreateInvoiceResponse = serde_json::from_str(&text)
            .map_err(|e| format!("xendit parse error: {e} / body={text}"))?;
        info!(invoice_id=%parsed.id, status=%parsed.status, "xendit invoice created");
        Ok(parsed)
    }

    /// Fetch a Xendit invoice by its Xendit-assigned invoice id.
    /// Used for status polling when we haven't received a webhook
    /// (the webhook URL isn't publicly reachable in dev/test/staging).
    /// Docs: https://api.xendit.co/v2/invoices/{id}
    pub async fn get_invoice(&self, invoice_id: &str) -> Result<GetInvoiceResponse, String> {
        if self.api_key.is_empty() {
            return Err("XENDIT_API_KEY not configured".into());
        }
        let auth = format!("Basic {}", B64.encode(format!("{}:", self.api_key)));
        let res = self
            .http
            .get(format!("{}/v2/invoices/{}", self.api_url, invoice_id))
            .header("Authorization", auth)
            .send()
            .await
            .map_err(|e| format!("xendit get_invoice failed: {e}"))?;

        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            warn!(%status, "xendit get_invoice error body={}", text);
            return Err(format!("xendit get_invoice error {status}: {text}"));
        }
        let parsed: GetInvoiceResponse = serde_json::from_str(&text)
            .map_err(|e| format!("xendit get_invoice parse error: {e} / body={text}"))?;
        Ok(parsed)
    }
}

pub struct CreateInvoiceRequest<'a> {
    pub external_id: &'a str,
    pub amount: i64,
    pub currency: &'a str,
    pub description: &'a str,
    pub payer_email: &'a str,
    pub customer_name: &'a str,
    pub customer_phone: &'a str,
    pub invoice_duration_seconds: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateInvoiceResponse {
    pub id: String,
    pub status: String,
    pub invoice_url: String,
    #[serde(default)]
    pub expiry_date: Option<String>,
}

/// Shape of Xendit's GET /v2/invoices/{id} response — we only deserialize
/// the fields we actually act on.
#[derive(Debug, Serialize, Deserialize)]
pub struct GetInvoiceResponse {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub payment_method: Option<String>,
    #[serde(default)]
    pub payment_channel: Option<String>,
    #[serde(default)]
    pub paid_at: Option<String>,
    #[serde(default)]
    pub payment_id: Option<String>,
}

/// Verify that a webhook request is authentic.
/// Xendit uses a static shared "x-callback-token" header (simpler model than HMAC).
pub fn verify_webhook(header_token: &str, configured_token: &str) -> bool {
    !configured_token.is_empty() && header_token == configured_token
}
