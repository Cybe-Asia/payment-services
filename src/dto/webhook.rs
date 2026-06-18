use serde::Deserialize;

/// Xendit invoice webhook payload. We only read the fields we actually use —
/// Xendit sends a larger envelope but serde ignores unknown fields.
#[derive(Deserialize, Debug)]
pub struct XenditInvoiceWebhook {
    pub external_id: String,
    pub status: String,
    #[serde(default)]
    pub payment_method: Option<String>,
    #[serde(default)]
    pub payment_id: Option<String>,
}
