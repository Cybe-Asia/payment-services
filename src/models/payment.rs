use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Payment — generic payment record, per graph-model §4.8.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct Payment {
    #[serde(rename = "paymentId")]
    pub payment_id: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "paymentType")]
    pub payment_type: String,
    pub status: String,
    pub amount: i64,
    pub currency: String,
    #[serde(rename = "paymentMethod")]
    pub payment_method: Option<String>,
    #[serde(rename = "gatewayRef")]
    pub gateway_ref: Option<String>,
    #[serde(rename = "invoiceRef")]
    pub invoice_ref: Option<String>,
    #[serde(rename = "hostedInvoiceUrl")]
    pub hosted_invoice_url: Option<String>,
    #[serde(rename = "receiptRef")]
    pub receipt_ref: Option<String>,
    #[serde(rename = "paidAt")]
    pub paid_at: Option<String>,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
    #[serde(rename = "leadId")]
    pub lead_id: Option<String>,
}
