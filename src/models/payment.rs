use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Payment — generic payment record, per graph-model §4.8.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
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
    #[serde(rename = "manualReference")]
    pub manual_reference: Option<String>,
    #[serde(rename = "amountSubmitted")]
    pub amount_submitted: Option<i64>,
    #[serde(rename = "amountVerified")]
    pub amount_verified: Option<i64>,
    #[serde(rename = "shortAmount")]
    pub short_amount: Option<i64>,
    #[serde(rename = "overpaidAmount")]
    pub overpaid_amount: Option<i64>,
    #[serde(rename = "manualBankAccountId")]
    pub manual_bank_account_id: Option<String>,
    #[serde(rename = "bankName")]
    pub bank_name: Option<String>,
    #[serde(rename = "bankAccountName")]
    pub bank_account_name: Option<String>,
    #[serde(rename = "bankAccountNumber")]
    pub bank_account_number: Option<String>,
    #[serde(rename = "reviewNote")]
    pub review_note: Option<String>,
    #[serde(rename = "rejectionReason")]
    pub rejection_reason: Option<String>,
    #[serde(rename = "reviewedBy")]
    pub reviewed_by: Option<String>,
    #[serde(rename = "reviewedAt")]
    pub reviewed_at: Option<String>,
}
