use super::model::Invoice;
use crate::services::payment_document::{render_payment_document, PaymentDocumentKind};

pub fn render(inv: &Invoice, receipt: bool) -> Result<Vec<u8>, String> {
    let s = &inv.snapshot;
    if receipt && inv.status != "paid" {
        return Err("Receipt is unavailable".into());
    }
    let payment=serde_json::from_value(serde_json::json!({
 "paymentId":s.id,"tenantId":s.tenant_id,"paymentType":"school_invoice","status":inv.status,"amount":s.amount,"currency":s.currency,"paymentMethod":"manual_transfer","receiptRef":inv.receipt_ref,"paidAt":inv.paid_at,"expiresAt":format!("{}T23:59:59+07:00",s.due_date),"bankName":s.bank.bank_name,"bankAccountName":s.bank.account_name,"bankAccountNumber":s.bank.account_number,
 "lineItemsJson":serde_json::to_string(&serde_json::json!([{ "label":format!("{} — {}",s.description,s.period),"amount":s.amount}])).map_err(|_|"Document unavailable")?
 })).map_err(|_|"Document unavailable")?;
    let context = crate::repositories::payment_repository::PaymentDocumentContext {
        payment,
        pricing_snapshot_json: None,
        parent_name: s.payer_name.clone(),
        parent_email: String::new(),
        parent_location: None,
        school_code: s.school_code.clone(),
        student_names: vec![s.student_name.clone()],
        created_at: Some(s.issued_at.clone()),
    };
    Ok(render_payment_document(
        &context,
        if receipt {
            PaymentDocumentKind::Receipt
        } else {
            PaymentDocumentKind::Invoice
        },
    ))
}
