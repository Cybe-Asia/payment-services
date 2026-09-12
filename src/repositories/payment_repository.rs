use neo4rs::{Graph, Query, Row};

use crate::models::payment::Payment;
use crate::models::payment_proof::PaymentProof;

const OFFER_PAYMENT_READY_STUDENT_STATUS: &str = "documents_verified";

#[derive(Clone, Debug)]
pub struct ManualBankDetails {
    pub bank_account_id: String,
    pub bank_name: String,
    pub account_name: String,
    pub account_number: String,
    pub instructions: String,
}

#[derive(Clone, Debug)]
pub struct CreateProofInput<'a> {
    pub payment_proof_id: &'a str,
    pub payment_id: &'a str,
    pub amount_submitted: i64,
    pub paid_at: Option<&'a str>,
    pub payer_name: Option<&'a str>,
    pub payer_bank: Option<&'a str>,
    pub reference_number: Option<&'a str>,
    pub object_key: &'a str,
    pub file_name: &'a str,
    pub mime_type: &'a str,
    pub size_bytes: i64,
    pub document_hash: &'a str,
    pub uploaded_by: &'a str,
    pub tenant_id: &'a str,
    pub lead_id: &'a str,
}

#[derive(Clone, Debug)]
pub struct AcceptedOfferSnapshot {
    pub offer_id: String,
    pub offer_revision: i64,
    pub lead_id: String,
    pub pricing_snapshot_hash: String,
    pub pricing_snapshot_json: String,
}

pub async fn init_doku_indexes(graph: &Graph) -> Result<(), neo4rs::Error> {
    graph
        .run(Query::new(
            "CREATE CONSTRAINT doku_webhook_request_unique IF NOT EXISTS FOR (r:DokuWebhookReceipt) REQUIRE r.request_id IS UNIQUE".to_string(),
        ))
        .await?;
    graph
        .run(Query::new(
            "CREATE CONSTRAINT offer_payment_slot_unique IF NOT EXISTS FOR (s:OfferPaymentSlot) REQUIRE s.slot_id IS UNIQUE".to_string(),
        ))
        .await?;
    Ok(())
}

pub async fn reserve_offer_payment_slot(
    graph: &Graph,
    tenant_id: &str,
    offer: &AcceptedOfferSnapshot,
    payment_id: &str,
    payment_method: &str,
    allow_terminal_handoff: bool,
) -> Result<bool, neo4rs::Error> {
    let slot_id = offer_payment_slot_id(tenant_id, offer);
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id, tenant_id:$tenant_id}), \
               (o:Offer {offer_id:$offer_id, tenant_id:$tenant_id}) \
         WHERE o.status='accepted' AND o.revision=$offer_revision \
           AND o.pricing_snapshot_hash=$snapshot_hash \
         MERGE (slot:OfferPaymentSlot {slot_id:$slot_id}) \
         ON CREATE SET slot.tenant_id=$tenant_id, slot.offer_id=$offer_id, \
            slot.offer_revision=$offer_revision, slot.pricing_snapshot_hash=$snapshot_hash, \
            slot.payment_id=$payment_id, slot.payment_method=$payment_method, \
            slot.created_at=datetime(), slot.updated_at=datetime() \
         WITH l, o, slot \
         OPTIONAL MATCH (previous:Payment {payment_id:slot.payment_id, tenant_id:$tenant_id}) \
         WITH l, o, slot, previous \
         WHERE (slot.payment_id=$payment_id AND slot.payment_method=$payment_method) \
            OR ($allow_terminal_handoff AND slot.payment_method='doku' \
                AND previous.status IN ['expired','failed','cancelled']) \
         SET slot.payment_id=$payment_id, slot.payment_method=$payment_method, \
             slot.updated_at=datetime() \
         MERGE (o)-[:HAS_PAYMENT_SLOT]->(slot) \
         RETURN slot.slot_id AS slot_id"
            .to_string(),
    )
    .param("lead_id", offer.lead_id.clone())
    .param("tenant_id", tenant_id.to_string())
    .param("offer_id", offer.offer_id.clone())
    .param("offer_revision", offer.offer_revision)
    .param("snapshot_hash", offer.pricing_snapshot_hash.clone())
    .param("slot_id", slot_id)
    .param("payment_id", payment_id.to_string())
    .param("payment_method", payment_method.to_string())
    .param("allow_terminal_handoff", allow_terminal_handoff);
    let mut result = graph.execute(q).await?;
    Ok(result.next().await?.is_some())
}

fn offer_payment_slot_id(tenant_id: &str, offer: &AcceptedOfferSnapshot) -> String {
    format!(
        "{}:{}:{}:{}",
        tenant_id, offer.offer_id, offer.offer_revision, offer.pricing_snapshot_hash
    )
}

pub fn manual_proof_upload_allowed(status: &str) -> bool {
    matches!(status, "awaiting_proof" | "underpaid" | "proof_rejected")
}

pub fn manual_review_allowed(status: &str) -> bool {
    status == "pending_verification"
}

pub async fn find_accepted_offer_snapshot(
    graph: &Graph,
    offer_id: &str,
    lead_ids: &[String],
    tenant_id: &str,
) -> Result<Option<AcceptedOfferSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead)-[:HAS_STUDENT]->(s:Student)-[:HAS_OFFER]->(o:Offer {offer_id:$offer_id}) \
         MATCH (o)-[:ACCEPTED_VIA]->(a:OfferAcceptance) \
         WHERE l.lead_id IN $lead_ids AND o.tenant_id = $tenant_id \
           AND (coalesce(s.applicantStatus,'') = $payment_ready_status \
             OR (coalesce(s.applicantStatus,'') = 'offer_accepted' AND EXISTS { \
               MATCH (s)-[:REQUIRES_DOCUMENT]->(:DocumentRequest {request_type:'application_document_pack', status:'approved'}) \
             })) \
           AND o.status = 'accepted' AND a.status = 'accepted' \
           AND a.offer_revision = o.revision \
           AND a.pricing_snapshot_hash = o.pricing_snapshot_hash \
           AND a.terms_hash = o.terms_hash \
         RETURN o.offer_id AS offer_id, o.revision AS offer_revision, l.lead_id AS lead_id, \
                o.pricing_snapshot_hash AS pricing_snapshot_hash, \
                o.pricing_snapshot_json AS pricing_snapshot_json LIMIT 1"
            .to_string(),
    )
    .param("offer_id", offer_id.to_string())
    .param("lead_ids", lead_ids.to_vec())
    .param("tenant_id", tenant_id.to_string())
    .param("payment_ready_status", OFFER_PAYMENT_READY_STUDENT_STATUS);
    let mut result = graph.execute(q).await?;
    Ok(result.next().await?.map(|row| AcceptedOfferSnapshot {
        offer_id: row.get("offer_id").unwrap_or_default(),
        offer_revision: row.get("offer_revision").unwrap_or(1),
        lead_id: row.get("lead_id").unwrap_or_default(),
        pricing_snapshot_hash: row.get("pricing_snapshot_hash").unwrap_or_default(),
        pricing_snapshot_json: row.get("pricing_snapshot_json").unwrap_or_default(),
    }))
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_doku_pending(
    graph: &Graph,
    payment_id: &str,
    tenant_id: &str,
    offer: &AcceptedOfferSnapshot,
    amount: i64,
    currency: &str,
    invoice_number: &str,
    request_id: &str,
    attempt: u32,
    checkout_url: &str,
    token_id: &str,
    session_id: Option<&str>,
) -> Result<bool, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id, tenant_id:$tenant_id}), \
               (o:Offer {offer_id:$offer_id, tenant_id:$tenant_id}) \
         WHERE o.status='accepted' AND o.revision=$offer_revision \
           AND o.pricing_snapshot_hash=$snapshot_hash \
         MATCH (o)-[:HAS_PAYMENT_SLOT]->(slot:OfferPaymentSlot {payment_id:$payment_id, payment_method:'doku'}) \
         WHERE slot.pricing_snapshot_hash=$snapshot_hash \
         OPTIONAL MATCH (o)-[:PAID_VIA]->(active:Payment) \
         WHERE NOT active.status IN ['expired','failed','cancelled'] \
         WITH l, o, [payment IN collect(active) WHERE payment IS NOT NULL] AS active_payments \
         WHERE size(active_payments) = 0 OR all(payment IN active_payments WHERE payment.payment_id = $payment_id) \
         MERGE (f:FeeObligation {offer_id:$offer_id, obligation_type:'offer_due_now'}) \
         ON CREATE SET f.fee_obligation_id=$fee_id, f.tenant_id=$tenant_id, \
            f.amount_due=$amount, f.currency=$currency, f.status='outstanding', \
            f.pricing_snapshot_hash=$snapshot_hash, f.created_at=datetime() \
         MERGE (p:Payment {payment_id:$payment_id}) \
         ON CREATE SET p.invoice_email_status='queued', p.tenant_id=$tenant_id, p.payment_type='offer_due_now', \
            p.status='pending', p.amount=$amount, p.net_amount=$amount, p.currency=$currency, \
            p.payment_method='doku', p.provider='doku', p.invoice_ref=$invoice_number, \
            p.gateway_ref=$token_id, p.doku_session_id=$session_id, p.doku_request_id=$request_id, \
            p.provider_attempt=$attempt, \
            p.hosted_invoice_url=$checkout_url, p.offer_id=$offer_id, \
            p.offer_revision=$offer_revision, p.pricing_snapshot_hash=$snapshot_hash, \
            p.created_at=datetime(), p.updated_at=datetime() \
         MERGE (l)-[:MADE_PAYMENT]->(p) \
         MERGE (o)-[:PAID_VIA]->(p) \
         MERGE (f)-[:SETTLED_BY]->(p) RETURN p"
            .to_string(),
    )
    .param("lead_id", offer.lead_id.clone())
    .param("offer_id", offer.offer_id.clone())
    .param("offer_revision", offer.offer_revision)
    .param("snapshot_hash", offer.pricing_snapshot_hash.clone())
    .param(
        "fee_id",
        format!(
            "FEEOBL-OFFER-{}",
            &offer.pricing_snapshot_hash[..24.min(offer.pricing_snapshot_hash.len())]
        ),
    )
    .param("payment_id", payment_id.to_string())
    .param("tenant_id", tenant_id.to_string())
    .param("amount", amount)
    .param("currency", currency.to_string())
    .param("invoice_number", invoice_number.to_string())
    .param("request_id", request_id.to_string())
    .param("attempt", attempt as i64)
    .param("checkout_url", checkout_url.to_string())
    .param("token_id", token_id.to_string())
    .param("session_id", session_id.unwrap_or("").to_string());
    let mut result = graph.execute(q).await?;
    Ok(result.next().await?.is_some())
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_offer_manual_pending(
    graph: &Graph,
    payment_id: &str,
    tenant_id: &str,
    offer: &AcceptedOfferSnapshot,
    amount: i64,
    currency: &str,
    expires_iso: &str,
    manual_reference: &str,
    bank: &ManualBankDetails,
) -> Result<bool, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id, tenant_id:$tenant_id}), \
               (o:Offer {offer_id:$offer_id, tenant_id:$tenant_id}) \
         WHERE o.status='accepted' AND o.revision=$offer_revision \
           AND o.pricing_snapshot_hash=$snapshot_hash \
         MATCH (o)-[:HAS_PAYMENT_SLOT]->(slot:OfferPaymentSlot {payment_id:$payment_id, payment_method:'manual_transfer'}) \
         WHERE slot.pricing_snapshot_hash=$snapshot_hash \
         OPTIONAL MATCH (o)-[:PAID_VIA]->(active:Payment) \
         WHERE NOT active.status IN ['expired','failed','cancelled'] \
         WITH l, o, [candidate IN collect(active) WHERE candidate IS NOT NULL] AS active_payments \
         WHERE size(active_payments) = 0 OR all(candidate IN active_payments WHERE candidate.payment_id=$payment_id) \
         MERGE (f:FeeObligation {offer_id:$offer_id, obligation_type:'offer_due_now'}) \
         ON CREATE SET f.fee_obligation_id=$fee_id, f.tenant_id=$tenant_id, \
            f.amount_due=$amount, f.currency=$currency, f.status='outstanding', \
            f.pricing_snapshot_hash=$snapshot_hash, f.created_at=datetime() \
         MERGE (p:Payment {payment_id:$payment_id}) \
         ON CREATE SET p.invoice_email_status='queued', p.tenant_id=$tenant_id, p.payment_type='offer_due_now', \
            p.status='awaiting_proof', p.amount=$amount, p.net_amount=$amount, \
            p.currency=$currency, p.payment_method='manual_transfer', p.provider='manual_transfer', \
            p.manual_reference=$manual_reference, \
            p.manual_bank_account_id=$manual_bank_account_id, p.bank_name=$bank_name, \
            p.bank_account_name=$account_name, p.bank_account_number=$account_number, \
            p.manual_instructions=$instructions, p.amount_submitted=0, p.amount_verified=0, \
            p.short_amount=$amount, p.overpaid_amount=0, p.expires_at=datetime($expires_iso), \
            p.offer_id=$offer_id, p.offer_revision=$offer_revision, \
            p.pricing_snapshot_hash=$snapshot_hash, p.created_at=datetime(), p.updated_at=datetime() \
         MERGE (l)-[:MADE_PAYMENT]->(p) \
         MERGE (o)-[:PAID_VIA]->(p) \
         MERGE (f)-[:SETTLED_BY]->(p) \
         RETURN p.payment_id AS payment_id"
            .to_string(),
    )
    .param("lead_id", offer.lead_id.clone())
    .param("offer_id", offer.offer_id.clone())
    .param("offer_revision", offer.offer_revision)
    .param("snapshot_hash", offer.pricing_snapshot_hash.clone())
    .param(
        "fee_id",
        format!(
            "FEEOBL-OFFER-{}",
            &offer.pricing_snapshot_hash[..24.min(offer.pricing_snapshot_hash.len())]
        ),
    )
    .param("payment_id", payment_id.to_string())
    .param("tenant_id", tenant_id.to_string())
    .param("amount", amount)
    .param("currency", currency.to_string())
    .param("expires_iso", expires_iso.to_string())
    .param("manual_reference", manual_reference.to_string())
    .param("manual_bank_account_id", bank.bank_account_id.clone())
    .param("bank_name", bank.bank_name.clone())
    .param("account_name", bank.account_name.clone())
    .param("account_number", bank.account_number.clone())
    .param("instructions", bank.instructions.clone());
    let mut result = graph.execute(q).await?;
    Ok(result.next().await?.is_some())
}

pub async fn find_by_invoice_ref(
    graph: &Graph,
    invoice_ref: &str,
) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.invoice_ref = $val", invoice_ref, None).await
}

pub async fn find_doku_request_id(
    graph: &Graph,
    payment_id: &str,
) -> Result<Option<String>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id, provider:'doku'}) \
         RETURN p.doku_request_id AS request_id LIMIT 1"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string());
    let mut result = graph.execute(q).await?;
    Ok(result
        .next()
        .await?
        .and_then(|row| row.get::<String>("request_id"))
        .filter(|value| !value.is_empty()))
}

/// Records provider delivery exactly once and performs the authoritative paid
/// transition only for a previously pending DOKU payment.
pub async fn apply_doku_webhook(
    graph: &Graph,
    request_id: &str,
    payment_id: &str,
    normalized_status: &str,
    provider_reference: Option<&str>,
) -> Result<bool, neo4rs::Error> {
    let q = Query::new(
        "MERGE (r:DokuWebhookReceipt {request_id:$request_id}) \
         ON CREATE SET r.payment_id=$payment_id, r.status=$status, r.processed=false, r.received_at=datetime() \
         WITH r WHERE r.processed=false \
         MATCH (p:Payment {payment_id:$payment_id, provider:'doku'}) \
         WITH r, p, p.status AS previous_status \
         SET p.status = CASE WHEN $status='paid' AND p.status='pending' THEN 'paid' \
                             WHEN $status IN ['expired','failed','cancelled'] AND p.status='pending' THEN $status \
                             ELSE p.status END, \
             p.receipt_email_status = CASE WHEN $status='paid' AND previous_status='pending' AND p.payment_type='application_fee' THEN coalesce(p.receipt_email_status,'queued') ELSE p.receipt_email_status END, \
             p.paid_at = CASE WHEN $status='paid' AND previous_status='pending' THEN datetime() ELSE p.paid_at END, \
             p.receipt_ref = coalesce($provider_reference, p.receipt_ref), p.updated_at=datetime(), \
             r.processed=true, r.processed_at=datetime() \
         WITH p, r, previous_status \
         OPTIONAL MATCH (f:FeeObligation)-[:SETTLED_BY]->(p) \
         SET f.status = CASE WHEN p.status='paid' THEN 'settled' ELSE f.status END, \
             f.settled_at = CASE WHEN p.status='paid' THEN datetime() ELSE f.settled_at END \
         RETURN previous_status='pending' AND $status <> 'pending' AS applied"
            .to_string(),
    )
    .param("request_id", request_id.to_string())
    .param("payment_id", payment_id.to_string())
    .param("status", normalized_status.to_string())
    .param("provider_reference", provider_reference.unwrap_or("").to_string());
    let mut result = graph.execute(q).await?;
    Ok(result
        .next()
        .await?
        .and_then(|row| row.get::<bool>("applied"))
        .unwrap_or(false))
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PaymentReviewRow {
    #[serde(rename = "paymentId")]
    pub payment_id: String,
    #[serde(rename = "leadId")]
    pub lead_id: String,
    #[serde(rename = "parentName")]
    pub parent_name: String,
    #[serde(rename = "parentEmail")]
    pub parent_email: String,
    pub school: String,
    #[serde(rename = "paymentType")]
    pub payment_type: String,
    pub status: String,
    pub amount: i64,
    pub currency: String,
    #[serde(rename = "amountSubmitted")]
    pub amount_submitted: Option<i64>,
    #[serde(rename = "amountVerified")]
    pub amount_verified: Option<i64>,
    #[serde(rename = "shortAmount")]
    pub short_amount: Option<i64>,
    #[serde(rename = "latestProofId")]
    pub latest_proof_id: Option<String>,
    #[serde(rename = "latestProofFileName")]
    pub latest_proof_file_name: Option<String>,
    #[serde(rename = "latestProofAmount")]
    pub latest_proof_amount: Option<i64>,
    #[serde(rename = "latestProofUploadedAt")]
    pub latest_proof_uploaded_at: Option<String>,
    #[serde(rename = "latestProofPaidAt", skip_serializing_if = "Option::is_none")]
    pub latest_proof_paid_at: Option<String>,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "paidAt", skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<String>,
    #[serde(rename = "reviewedAt", skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
    #[serde(rename = "activityAt", skip_serializing_if = "Option::is_none")]
    pub activity_at: Option<String>,
    #[serde(rename = "ageDays")]
    pub age_days: Option<i64>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PaymentReviewLead {
    #[serde(rename = "leadId")]
    pub lead_id: String,
    #[serde(rename = "parentName")]
    pub parent_name: String,
    #[serde(rename = "parentEmail")]
    pub parent_email: String,
    pub school: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PaymentReviewDetail {
    pub payment: Payment,
    pub lead: PaymentReviewLead,
    pub proofs: Vec<PaymentProof>,
}

#[derive(Clone, Copy, Debug)]
pub struct PaymentReviewFilters<'a> {
    pub status: &'a str,
    pub school: &'a str,
    pub search: &'a str,
    pub date_from: &'a str,
    pub date_to: &'a str,
    /// Whitelisted sort key + direction (additive; default ordering preserved).
    pub sort: &'a str,
    pub sort_dir: &'a str,
}

/// Sortable columns for the manual payment review queue (key → RETURN alias).
const REVIEW_SORT_COLUMNS: &[(&str, &str)] = &[
    ("parent", "parent_name"),
    ("payment", "payment_type"),
    ("due", "amount"),
    ("submitted", "amount_submitted"),
    ("verified", "amount_verified"),
    ("status", "status"),
    ("proof_age", "age_days"),
];

/// Builds a safe ORDER BY for the review queue. Unknown/absent sort preserves
/// the default ordering exactly; user input only ever selects a whitelist
/// entry, so there is no Cypher injection surface.
fn review_order_by(sort: &str, dir: &str) -> String {
    match REVIEW_SORT_COLUMNS.iter().find(|(key, _)| *key == sort) {
        Some((_, expr)) => {
            let direction = if dir == "asc" { "ASC" } else { "DESC" };
            format!("ORDER BY {} {}", expr, direction)
        }
        None => "ORDER BY activity_at ASC".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn create_pending(
    graph: &Graph,
    payment_id: &str,
    tenant_id: &str,
    payment_type: &str,
    amount: i64,
    currency: &str,
    gateway_ref: &str,
    invoice_ref: &str,
    hosted_invoice_url: &str,
    expires_iso: &str,
    fee_obligation_id: &str,
    lead_id: &str,
    gross_amount: i64,
    discount_amount: i64,
    promotion_code: Option<&str>,
    promotion_rule_id: Option<&str>,
    promotion_snapshot_json: Option<&str>,
    line_items_json: &str,
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}), (f:FeeObligation {fee_obligation_id:$fid}) \
         CREATE (p:Payment { \
            payment_id:$payment_id, tenant_id:$tenant_id, payment_type:$payment_type, \
            status:'pending', invoice_email_status:'queued', amount:$amount, currency:$currency, \
            gross_amount:$gross_amount, discount_amount:$discount_amount, net_amount:$amount, \
            promotion_code:$promotion_code, promotion_rule_id:$promotion_rule_id, \
            promotion_snapshot_json:$promotion_snapshot_json, line_items_json:$line_items_json, \
            gateway_ref:$gateway_ref, invoice_ref:$invoice_ref, \
            hosted_invoice_url:$hosted_invoice_url, \
            expires_at:datetime($expires_iso), created_at:datetime() \
         }) \
         MERGE (l)-[:MADE_PAYMENT]->(p) \
         MERGE (f)-[:SETTLED_BY]->(p) \
         RETURN p"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("tenant_id", tenant_id.to_string())
    .param("payment_type", payment_type.to_string())
    .param("amount", amount)
    .param("currency", currency.to_string())
    .param("gateway_ref", gateway_ref.to_string())
    .param("invoice_ref", invoice_ref.to_string())
    .param("hosted_invoice_url", hosted_invoice_url.to_string())
    .param("expires_iso", expires_iso.to_string())
    .param("fid", fee_obligation_id.to_string())
    .param("lead_id", lead_id.to_string())
    .param("gross_amount", gross_amount)
    .param("discount_amount", discount_amount)
    .param("promotion_code", promotion_code.unwrap_or("").to_string())
    .param(
        "promotion_rule_id",
        promotion_rule_id.unwrap_or("").to_string(),
    )
    .param(
        "promotion_snapshot_json",
        promotion_snapshot_json.unwrap_or("").to_string(),
    )
    .param("line_items_json", line_items_json.to_string());

    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create_manual_pending(
    graph: &Graph,
    payment_id: &str,
    tenant_id: &str,
    payment_type: &str,
    amount: i64,
    currency: &str,
    expires_iso: &str,
    fee_obligation_id: &str,
    lead_id: &str,
    manual_reference: &str,
    bank: &ManualBankDetails,
    gross_amount: i64,
    discount_amount: i64,
    promotion_code: Option<&str>,
    promotion_rule_id: Option<&str>,
    promotion_snapshot_json: Option<&str>,
    line_items_json: &str,
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}), (f:FeeObligation {fee_obligation_id:$fid}) \
         CREATE (p:Payment { \
            payment_id:$payment_id, tenant_id:$tenant_id, payment_type:$payment_type, \
            status:'awaiting_proof', invoice_email_status:'queued', amount:$amount, currency:$currency, \
            gross_amount:$gross_amount, discount_amount:$discount_amount, net_amount:$amount, \
            promotion_code:$promotion_code, promotion_rule_id:$promotion_rule_id, \
            promotion_snapshot_json:$promotion_snapshot_json, line_items_json:$line_items_json, \
            payment_method:'manual_transfer', manual_reference:$manual_reference, \
            manual_bank_account_id:$manual_bank_account_id, \
            bank_name:$bank_name, bank_account_name:$account_name, \
            bank_account_number:$account_number, manual_instructions:$instructions, \
            amount_submitted:0, amount_verified:0, short_amount:$amount, overpaid_amount:0, \
            expires_at:datetime($expires_iso), created_at:datetime(), updated_at:datetime() \
         }) \
         MERGE (l)-[:MADE_PAYMENT]->(p) \
         MERGE (f)-[:SETTLED_BY]->(p) \
         RETURN p"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("tenant_id", tenant_id.to_string())
    .param("payment_type", payment_type.to_string())
    .param("amount", amount)
    .param("currency", currency.to_string())
    .param("expires_iso", expires_iso.to_string())
    .param("fid", fee_obligation_id.to_string())
    .param("lead_id", lead_id.to_string())
    .param("manual_reference", manual_reference.to_string())
    .param("manual_bank_account_id", bank.bank_account_id.clone())
    .param("bank_name", bank.bank_name.clone())
    .param("account_name", bank.account_name.clone())
    .param("account_number", bank.account_number.clone())
    .param("instructions", bank.instructions.clone())
    .param("gross_amount", gross_amount)
    .param("discount_amount", discount_amount)
    .param("promotion_code", promotion_code.unwrap_or("").to_string())
    .param(
        "promotion_rule_id",
        promotion_rule_id.unwrap_or("").to_string(),
    )
    .param(
        "promotion_snapshot_json",
        promotion_snapshot_json.unwrap_or("").to_string(),
    )
    .param("line_items_json", line_items_json.to_string());

    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

pub async fn update_manual_bank_details(
    graph: &Graph,
    payment_id: &str,
    bank: &ManualBankDetails,
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id}) \
         WHERE p.payment_method = 'manual_transfer' \
           AND coalesce(p.status, '') IN ['awaiting_proof', 'underpaid', 'proof_rejected'] \
         SET p.manual_bank_account_id = $manual_bank_account_id, \
             p.bank_name = $bank_name, \
             p.bank_account_name = $account_name, \
             p.bank_account_number = $account_number, \
             p.manual_instructions = $instructions, \
             p.updated_at = datetime() \
         RETURN p"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("manual_bank_account_id", bank.bank_account_id.clone())
    .param("bank_name", bank.bank_name.clone())
    .param("account_name", bank.account_name.clone())
    .param("account_number", bank.account_number.clone())
    .param("instructions", bank.instructions.clone());

    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

pub async fn find_active_manual_for_lead(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<Payment>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (:Lead {lead_id:$lead_id})-[:MADE_PAYMENT]->(p:Payment {payment_type:$payment_type}) \
         WHERE p.payment_method = 'manual_transfer' \
           AND NOT coalesce(p.status, '') IN ['paid', 'expired', 'failed', 'cancelled'] \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         RETURN p.payment_id AS payment_id, p.tenant_id AS tenant_id, \
                p.payment_type AS payment_type, p.status AS status, \
                p.amount AS amount, p.currency AS currency, \
                p.gross_amount AS gross_amount, p.discount_amount AS discount_amount, \
                p.net_amount AS net_amount, p.promotion_code AS promotion_code, \
                p.promotion_rule_id AS promotion_rule_id, \
                p.promotion_snapshot_json AS promotion_snapshot_json, \
                p.line_items_json AS line_items_json, \
                p.payment_method AS payment_method, p.gateway_ref AS gateway_ref, \
                p.invoice_ref AS invoice_ref, p.hosted_invoice_url AS hosted_invoice_url, \
                p.receipt_ref AS receipt_ref, p.manual_reference AS manual_reference, \
                p.amount_submitted AS amount_submitted, p.amount_verified AS amount_verified, \
                p.short_amount AS short_amount, p.overpaid_amount AS overpaid_amount, \
                p.manual_bank_account_id AS manual_bank_account_id, \
                p.bank_name AS bank_name, p.bank_account_name AS bank_account_name, \
                p.bank_account_number AS bank_account_number, p.review_note AS review_note, \
                p.rejection_reason AS rejection_reason, p.reviewed_by AS reviewed_by, \
                toString(p.reviewed_at) AS reviewed_at, \
                toString(p.paid_at) AS paid_at, toString(p.expires_at) AS expires_at, \
                l.lead_id AS lead_id \
         ORDER BY p.created_at DESC \
         LIMIT 1".to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some(payment_from_row(&row)))
    } else {
        Ok(None)
    }
}

pub async fn find_by_id(graph: &Graph, payment_id: &str) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.payment_id = $val", payment_id, None).await
}

pub async fn find_by_id_for_tenant(
    graph: &Graph,
    payment_id: &str,
    tenant_id: &str,
) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.payment_id = $val", payment_id, Some(tenant_id)).await
}

#[allow(dead_code)]
pub async fn find_by_gateway_ref(
    graph: &Graph,
    gateway_ref: &str,
) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.gateway_ref = $val", gateway_ref, None).await
}

async fn find_by(
    graph: &Graph,
    predicate: &str,
    val: &str,
    tenant_id: Option<&str>,
) -> Result<Option<Payment>, neo4rs::Error> {
    let cypher = format!(
        "MATCH (p:Payment) WHERE {predicate} \
           AND ($tenant_id = '' OR p.tenant_id = $tenant_id) \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         RETURN p.payment_id AS payment_id, p.tenant_id AS tenant_id, \
                p.payment_type AS payment_type, p.status AS status, \
                p.amount AS amount, p.currency AS currency, \
                p.gross_amount AS gross_amount, p.discount_amount AS discount_amount, \
                p.net_amount AS net_amount, p.promotion_code AS promotion_code, \
                p.promotion_rule_id AS promotion_rule_id, \
                p.promotion_snapshot_json AS promotion_snapshot_json, \
                p.line_items_json AS line_items_json, \
                p.payment_method AS payment_method, p.gateway_ref AS gateway_ref, \
                p.invoice_ref AS invoice_ref, p.hosted_invoice_url AS hosted_invoice_url, \
                p.receipt_ref AS receipt_ref, p.manual_reference AS manual_reference, \
                p.amount_submitted AS amount_submitted, p.amount_verified AS amount_verified, \
                p.short_amount AS short_amount, p.overpaid_amount AS overpaid_amount, \
                p.manual_bank_account_id AS manual_bank_account_id, \
                p.bank_name AS bank_name, p.bank_account_name AS bank_account_name, \
                p.bank_account_number AS bank_account_number, p.review_note AS review_note, \
                p.rejection_reason AS rejection_reason, p.reviewed_by AS reviewed_by, \
                toString(p.reviewed_at) AS reviewed_at, \
                toString(p.paid_at) AS paid_at, toString(p.expires_at) AS expires_at, \
                l.lead_id AS lead_id \
         LIMIT 1"
    );
    let q = Query::new(cypher)
        .param("val", val.to_string())
        .param("tenant_id", tenant_id.unwrap_or("").to_string());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some(payment_from_row(&row)))
    } else {
        Ok(None)
    }
}

fn payment_from_row(row: &Row) -> Payment {
    Payment {
        payment_id: row.get("payment_id").unwrap_or_default(),
        tenant_id: row.get("tenant_id").unwrap_or_default(),
        payment_type: row.get("payment_type").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        amount: row.get::<i64>("amount").unwrap_or_default(),
        gross_amount: row.get("gross_amount"),
        discount_amount: row.get("discount_amount"),
        net_amount: row.get("net_amount"),
        promotion_code: row.get("promotion_code"),
        promotion_rule_id: row.get("promotion_rule_id"),
        promotion_snapshot_json: row.get("promotion_snapshot_json"),
        line_items_json: row.get("line_items_json"),
        currency: row.get("currency").unwrap_or_default(),
        payment_method: row.get("payment_method"),
        gateway_ref: row.get("gateway_ref"),
        invoice_ref: row.get("invoice_ref"),
        hosted_invoice_url: row.get("hosted_invoice_url"),
        receipt_ref: row.get("receipt_ref"),
        paid_at: row.get("paid_at"),
        expires_at: row.get("expires_at"),
        lead_id: row.get("lead_id"),
        manual_reference: row.get("manual_reference"),
        amount_submitted: row.get("amount_submitted"),
        amount_verified: row.get("amount_verified"),
        short_amount: row.get("short_amount"),
        overpaid_amount: row.get("overpaid_amount"),
        manual_bank_account_id: row.get("manual_bank_account_id"),
        bank_name: row.get("bank_name"),
        bank_account_name: row.get("bank_account_name"),
        bank_account_number: row.get("bank_account_number"),
        review_note: row.get("review_note"),
        rejection_reason: row.get("rejection_reason"),
        reviewed_by: row.get("reviewed_by"),
        reviewed_at: row.get("reviewed_at"),
    }
}

pub async fn mark_paid(
    graph: &Graph,
    payment_id: &str,
    payment_method: Option<&str>,
    receipt_ref: Option<&str>,
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id}) \
         WITH p, p.status AS previous_status \
         SET p.status = 'paid', p.paid_at = CASE WHEN previous_status='paid' THEN p.paid_at ELSE datetime() END, \
             p.receipt_email_status = CASE WHEN previous_status='pending' AND p.payment_type='application_fee' THEN coalesce(p.receipt_email_status,'queued') ELSE p.receipt_email_status END, \
             p.payment_method = coalesce($method, p.payment_method), \
             p.receipt_ref = coalesce($receipt, p.receipt_ref) \
         WITH p \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         SET l.status = 'paid', \
             l.setup_step = CASE \
               WHEN coalesce(l.setup_step, '') IN ['test_booked','test_completed','documents_requested','documents_complete','offer_pending','closed'] \
                 THEN l.setup_step \
               ELSE 'application_fee_paid' \
             END \
         RETURN p"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param(
        "method",
        payment_method.map(|s| s.to_string()).unwrap_or_default(),
    )
    .param(
        "receipt",
        receipt_ref.map(|s| s.to_string()).unwrap_or_default(),
    );
    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

pub async fn mark_status(
    graph: &Graph,
    payment_id: &str,
    status: &str,
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id}) SET p.status = $status RETURN p".to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("status", status.to_string());
    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

pub async fn has_duplicate_proof_hash(
    graph: &Graph,
    payment_id: &str,
    document_hash: &str,
) -> Result<bool, neo4rs::Error> {
    let q = Query::new(
        "MATCH (:Payment {payment_id:$payment_id})-[:HAS_PROOF]->(proof:PaymentProof {document_hash:$hash}) \
         WHERE coalesce(proof.status, '') <> 'rejected' \
         RETURN proof.payment_proof_id AS id \
         LIMIT 1".to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("hash", document_hash.to_string());
    let mut result = graph.execute(q).await?;
    Ok(result.next().await?.is_some())
}

pub async fn create_payment_proof(
    graph: &Graph,
    input: CreateProofInput<'_>,
) -> Result<PaymentProof, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id, tenant_id:$tenant_id})-[:MADE_PAYMENT]->\
               (p:Payment {payment_id:$payment_id, tenant_id:$tenant_id, payment_method:'manual_transfer'}) \
         WHERE p.status IN ['awaiting_proof','underpaid','proof_rejected'] \
         CREATE (proof:PaymentProof { \
            payment_proof_id:$payment_proof_id, status:'uploaded', \
            amount_submitted:$amount_submitted, paid_at:$paid_at, \
            payer_name:$payer_name, payer_bank:$payer_bank, reference_number:$reference_number, \
            object_key:$object_key, file_name:$file_name, mime_type:$mime_type, \
            size_bytes:$size_bytes, document_hash:$document_hash, \
            uploaded_by:$uploaded_by, uploaded_at:datetime(), created_at:datetime(), updated_at:datetime() \
         }) \
         MERGE (p)-[:HAS_PROOF]->(proof) \
         WITH p, proof \
         MATCH (p)-[:HAS_PROOF]->(allProof:PaymentProof) \
         WHERE coalesce(allProof.status, '') <> 'rejected' \
         WITH p, proof, sum(coalesce(allProof.amount_submitted, 0)) AS total_submitted \
         SET p.status = 'pending_verification', \
             p.amount_submitted = total_submitted, \
             p.short_amount = CASE \
                WHEN coalesce(p.amount_verified, 0) >= coalesce(p.amount, 0) THEN 0 \
                ELSE coalesce(p.amount, 0) - coalesce(p.amount_verified, 0) \
             END, \
             p.updated_at = datetime() \
         RETURN proof.payment_proof_id AS payment_proof_id, p.payment_id AS payment_id, \
                proof.status AS status, proof.amount_submitted AS amount_submitted, \
                proof.amount_verified AS amount_verified, toString(proof.paid_at) AS paid_at, \
                proof.payer_name AS payer_name, proof.payer_bank AS payer_bank, \
                proof.reference_number AS reference_number, proof.file_name AS file_name, \
                proof.mime_type AS mime_type, proof.size_bytes AS size_bytes, \
                proof.document_hash AS document_hash, proof.uploaded_by AS uploaded_by, \
                toString(proof.uploaded_at) AS uploaded_at, proof.reviewed_by AS reviewed_by, \
                toString(proof.reviewed_at) AS reviewed_at, proof.review_note AS review_note \
         LIMIT 1".to_string(),
    )
    .param("payment_id", input.payment_id.to_string())
    .param("tenant_id", input.tenant_id.to_string())
    .param("lead_id", input.lead_id.to_string())
    .param("payment_proof_id", input.payment_proof_id.to_string())
    .param("amount_submitted", input.amount_submitted)
    .param("paid_at", input.paid_at.unwrap_or("").to_string())
    .param("payer_name", input.payer_name.unwrap_or("").to_string())
    .param("payer_bank", input.payer_bank.unwrap_or("").to_string())
    .param("reference_number", input.reference_number.unwrap_or("").to_string())
    .param("object_key", input.object_key.to_string())
    .param("file_name", input.file_name.to_string())
    .param("mime_type", input.mime_type.to_string())
    .param("size_bytes", input.size_bytes)
    .param("document_hash", input.document_hash.to_string())
    .param("uploaded_by", input.uploaded_by.to_string());

    let mut result = graph.execute(q).await?;
    let Some(row) = result.next().await? else {
        return Ok(PaymentProof {
            payment_proof_id: String::new(),
            payment_id: input.payment_id.to_string(),
            status: String::new(),
            amount_submitted: 0,
            amount_verified: None,
            paid_at: None,
            payer_name: None,
            payer_bank: None,
            reference_number: None,
            file_name: String::new(),
            mime_type: String::new(),
            size_bytes: 0,
            document_hash: String::new(),
            uploaded_by: None,
            uploaded_at: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
        });
    };
    Ok(payment_proof_from_row(&row))
}

pub async fn find_payment_proof_object(
    graph: &Graph,
    proof_id: &str,
    tenant_id: &str,
) -> Result<Option<(String, String, String, Option<String>)>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {tenant_id:$tenant_id})-[:HAS_PROOF]->(proof:PaymentProof {payment_proof_id:$proof_id}) \
         OPTIONAL MATCH (l:Lead {tenant_id:$tenant_id})-[:MADE_PAYMENT]->(p) \
         RETURN proof.object_key AS object_key, proof.file_name AS file_name, \
                proof.mime_type AS mime_type, l.lead_id AS lead_id \
         LIMIT 1"
            .to_string(),
    )
    .param("proof_id", proof_id.to_string())
    .param("tenant_id", tenant_id.to_string());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some((
            row.get("object_key").unwrap_or_default(),
            row.get("file_name").unwrap_or_default(),
            row.get("mime_type")
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            row.get("lead_id"),
        )))
    } else {
        Ok(None)
    }
}

pub async fn list_review_rows(
    graph: &Graph,
    tenant_id: &str,
    filters: PaymentReviewFilters<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<PaymentReviewRow>, neo4rs::Error> {
    let order_sql = review_order_by(filters.sort, filters.sort_dir);
    let q = Query::new(format!(
        "MATCH (l:Lead {{tenant_id:$tenant_id}})-[:MADE_PAYMENT]->(p:Payment {{tenant_id:$tenant_id}}) \
         WHERE p.payment_method = 'manual_transfer' \
           AND ($status = '' OR p.status = $status) \
           AND ($school = '' OR toLower(coalesce(l.target_school_preference, '')) = toLower($school)) \
           AND ( \
             $search = '' OR \
             toLower(coalesce(l.parent_name, '')) CONTAINS toLower($search) OR \
             toLower(coalesce(l.email, '')) CONTAINS toLower($search) OR \
             toLower(p.payment_id) CONTAINS toLower($search) \
           ) \
         OPTIONAL MATCH (p)-[:HAS_PROOF]->(proof:PaymentProof) \
         WITH l, p, proof ORDER BY proof.uploaded_at DESC \
         WITH l, p, head(collect(proof)) AS proof \
         WITH l, p, proof, coalesce(p.paid_at, proof.uploaded_at, p.created_at) AS activity_at \
         WHERE ($date_from = '' OR activity_at >= datetime($date_from)) \
           AND ($date_to = '' OR activity_at <= datetime($date_to)) \
         RETURN p.payment_id AS payment_id, l.lead_id AS lead_id, \
                l.parent_name AS parent_name, l.email AS parent_email, \
                l.target_school_preference AS school, p.payment_type AS payment_type, \
                p.status AS status, p.amount AS amount, p.currency AS currency, \
                p.amount_submitted AS amount_submitted, p.amount_verified AS amount_verified, \
                p.short_amount AS short_amount, proof.payment_proof_id AS latest_proof_id, \
                proof.file_name AS latest_proof_file_name, proof.amount_submitted AS latest_proof_amount, \
                toString(proof.uploaded_at) AS latest_proof_uploaded_at, \
                toString(proof.paid_at) AS latest_proof_paid_at, \
                toString(p.created_at) AS created_at, toString(p.paid_at) AS paid_at, \
                toString(p.reviewed_at) AS reviewed_at, toString(activity_at) AS activity_at, \
                duration.inDays(activity_at, datetime()).days AS age_days \
         {order_sql} \
         SKIP $offset LIMIT $limit"
    ))
    .param("tenant_id", tenant_id.to_string())
    .param("status", filters.status.to_string())
    .param("school", filters.school.to_string())
    .param("search", filters.search.to_string())
    .param("date_from", filters.date_from.to_string())
    .param("date_to", filters.date_to.to_string())
    .param("limit", limit)
    .param("offset", offset);

    let mut result = graph.execute(q).await?;
    let mut rows = Vec::new();
    while let Some(row) = result.next().await? {
        rows.push(PaymentReviewRow {
            payment_id: row.get("payment_id").unwrap_or_default(),
            lead_id: row.get("lead_id").unwrap_or_default(),
            parent_name: row.get("parent_name").unwrap_or_default(),
            parent_email: row.get("parent_email").unwrap_or_default(),
            school: row.get("school").unwrap_or_default(),
            payment_type: row.get("payment_type").unwrap_or_default(),
            status: row.get("status").unwrap_or_default(),
            amount: row.get("amount").unwrap_or_default(),
            currency: row.get("currency").unwrap_or_else(|| "IDR".to_string()),
            amount_submitted: row.get("amount_submitted"),
            amount_verified: row.get("amount_verified"),
            short_amount: row.get("short_amount"),
            latest_proof_id: row.get("latest_proof_id"),
            latest_proof_file_name: row.get("latest_proof_file_name"),
            latest_proof_amount: row.get("latest_proof_amount"),
            latest_proof_uploaded_at: row.get("latest_proof_uploaded_at"),
            latest_proof_paid_at: row.get("latest_proof_paid_at"),
            created_at: row.get("created_at"),
            paid_at: row.get("paid_at"),
            reviewed_at: row.get("reviewed_at"),
            activity_at: row.get("activity_at"),
            age_days: row.get("age_days"),
        });
    }
    Ok(rows)
}

pub async fn count_review_rows(
    graph: &Graph,
    tenant_id: &str,
    filters: PaymentReviewFilters<'_>,
) -> Result<i64, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {tenant_id:$tenant_id})-[:MADE_PAYMENT]->(p:Payment {tenant_id:$tenant_id}) \
         WHERE p.payment_method = 'manual_transfer' \
           AND ($status = '' OR p.status = $status) \
           AND ($school = '' OR toLower(coalesce(l.target_school_preference, '')) = toLower($school)) \
           AND ( \
             $search = '' OR \
             toLower(coalesce(l.parent_name, '')) CONTAINS toLower($search) OR \
             toLower(coalesce(l.email, '')) CONTAINS toLower($search) OR \
             toLower(p.payment_id) CONTAINS toLower($search) \
           ) \
         OPTIONAL MATCH (p)-[:HAS_PROOF]->(proof:PaymentProof) \
         WITH l, p, proof ORDER BY proof.uploaded_at DESC \
         WITH l, p, head(collect(proof)) AS proof \
         WITH l, p, proof, coalesce(p.paid_at, proof.uploaded_at, p.created_at) AS activity_at \
         WHERE ($date_from = '' OR activity_at >= datetime($date_from)) \
           AND ($date_to = '' OR activity_at <= datetime($date_to)) \
         RETURN count(p) AS total".to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("status", filters.status.to_string())
    .param("school", filters.school.to_string())
    .param("search", filters.search.to_string())
    .param("date_from", filters.date_from.to_string())
    .param("date_to", filters.date_to.to_string());
    let mut result = graph.execute(q).await?;
    Ok(result
        .next()
        .await?
        .and_then(|row| row.get::<i64>("total"))
        .unwrap_or(0))
}

pub async fn find_review_detail(
    graph: &Graph,
    tenant_id: &str,
    payment_id: &str,
) -> Result<Option<PaymentReviewDetail>, neo4rs::Error> {
    let payment = match find_by_id_for_tenant(graph, payment_id, tenant_id).await? {
        Some(payment) => payment,
        None => return Ok(None),
    };

    let lead_id = payment.lead_id.clone().unwrap_or_default();
    let lead = fetch_review_lead(graph, tenant_id, payment_id)
        .await?
        .unwrap_or(PaymentReviewLead {
            lead_id,
            parent_name: String::new(),
            parent_email: String::new(),
            school: String::new(),
        });
    let proofs = list_proofs_for_payment(graph, tenant_id, payment_id).await?;

    Ok(Some(PaymentReviewDetail {
        payment,
        lead,
        proofs,
    }))
}

pub struct ManualPaymentReviewUpdate<'a> {
    pub payment_id: &'a str,
    pub tenant_id: &'a str,
    pub payment_status: &'a str,
    pub proof_status: &'a str,
    pub amount_verified: i64,
    pub short_amount: i64,
    pub overpaid_amount: i64,
    pub note: Option<&'a str>,
    pub rejection_reason: Option<&'a str>,
    pub reviewed_by: &'a str,
    pub receipt_ref: Option<&'a str>,
}

pub async fn review_manual_payment(
    graph: &Graph,
    input: ManualPaymentReviewUpdate<'_>,
) -> Result<bool, neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id, tenant_id:$tenant_id, payment_method:'manual_transfer'}) \
         WHERE p.status='pending_verification' \
         SET p.status = $payment_status, \
             p.amount_verified = $amount_verified, \
             p.short_amount = $short_amount, \
             p.overpaid_amount = $overpaid_amount, \
             p.review_note = $note, \
             p.rejection_reason = $rejection_reason, \
             p.reviewed_by = $reviewed_by, \
             p.reviewed_at = datetime(), \
             p.updated_at = datetime(), \
             p.payment_method = 'manual_transfer', \
             p.receipt_ref = CASE WHEN $receipt_ref = '' THEN p.receipt_ref ELSE $receipt_ref END \
         FOREACH (_ IN CASE WHEN $payment_status = 'paid' THEN [1] ELSE [] END | \
             SET p.paid_at = datetime(), p.receipt_email_status = CASE WHEN p.payment_type='application_fee' THEN coalesce(p.receipt_email_status,'queued') ELSE p.receipt_email_status END \
         ) \
         WITH p \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         FOREACH (_ IN CASE WHEN $payment_status = 'paid' AND l IS NOT NULL THEN [1] ELSE [] END | \
             SET l.status = 'paid', \
                 l.setup_step = CASE \
                   WHEN coalesce(l.setup_step, '') IN ['test_booked','test_completed','documents_requested','documents_complete','offer_pending','closed'] \
                     THEN l.setup_step \
                   ELSE 'application_fee_paid' \
                 END \
         ) \
         WITH p \
         OPTIONAL MATCH (p)-[:HAS_PROOF]->(proof:PaymentProof) \
         WHERE proof.status = 'uploaded' \
         SET proof.status = $proof_status, \
             proof.amount_verified = CASE WHEN $proof_status = 'rejected' THEN proof.amount_verified ELSE proof.amount_submitted END, \
             proof.review_note = $note, \
             proof.reviewed_by = $reviewed_by, \
             proof.reviewed_at = datetime(), \
             proof.updated_at = datetime() \
         RETURN p".to_string(),
    )
    .param("payment_id", input.payment_id.to_string())
    .param("tenant_id", input.tenant_id.to_string())
    .param("payment_status", input.payment_status.to_string())
    .param("proof_status", input.proof_status.to_string())
    .param("amount_verified", input.amount_verified)
    .param("short_amount", input.short_amount)
    .param("overpaid_amount", input.overpaid_amount)
    .param("note", input.note.unwrap_or("").to_string())
    .param(
        "rejection_reason",
        input.rejection_reason.unwrap_or("").to_string(),
    )
    .param("reviewed_by", input.reviewed_by.to_string())
    .param("receipt_ref", input.receipt_ref.unwrap_or("").to_string());

    let mut result = graph.execute(q).await?;
    Ok(result.next().await?.is_some())
}

async fn fetch_review_lead(
    graph: &Graph,
    tenant_id: &str,
    payment_id: &str,
) -> Result<Option<PaymentReviewLead>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {tenant_id:$tenant_id})-[:MADE_PAYMENT]->(:Payment {payment_id:$payment_id, tenant_id:$tenant_id}) \
         RETURN l.lead_id AS lead_id, l.parent_name AS parent_name, \
                l.email AS parent_email, l.target_school_preference AS school \
         LIMIT 1"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("tenant_id", tenant_id.to_string());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some(PaymentReviewLead {
            lead_id: row.get("lead_id").unwrap_or_default(),
            parent_name: row.get("parent_name").unwrap_or_default(),
            parent_email: row.get("parent_email").unwrap_or_default(),
            school: row.get("school").unwrap_or_default(),
        }))
    } else {
        Ok(None)
    }
}

async fn list_proofs_for_payment(
    graph: &Graph,
    tenant_id: &str,
    payment_id: &str,
) -> Result<Vec<PaymentProof>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id, tenant_id:$tenant_id})-[:HAS_PROOF]->(proof:PaymentProof) \
         RETURN proof.payment_proof_id AS payment_proof_id, p.payment_id AS payment_id, \
                proof.status AS status, proof.amount_submitted AS amount_submitted, \
                proof.amount_verified AS amount_verified, toString(proof.paid_at) AS paid_at, \
                proof.payer_name AS payer_name, proof.payer_bank AS payer_bank, \
                proof.reference_number AS reference_number, proof.file_name AS file_name, \
                proof.mime_type AS mime_type, proof.size_bytes AS size_bytes, \
                proof.document_hash AS document_hash, proof.uploaded_by AS uploaded_by, \
                toString(proof.uploaded_at) AS uploaded_at, proof.reviewed_by AS reviewed_by, \
                toString(proof.reviewed_at) AS reviewed_at, proof.review_note AS review_note \
         ORDER BY proof.uploaded_at DESC"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("tenant_id", tenant_id.to_string());
    let mut result = graph.execute(q).await?;
    let mut proofs = Vec::new();
    while let Some(row) = result.next().await? {
        proofs.push(payment_proof_from_row(&row));
    }
    Ok(proofs)
}

fn payment_proof_from_row(row: &Row) -> PaymentProof {
    PaymentProof {
        payment_proof_id: row.get("payment_proof_id").unwrap_or_default(),
        payment_id: row.get("payment_id").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        amount_submitted: row.get("amount_submitted").unwrap_or_default(),
        amount_verified: row.get("amount_verified"),
        paid_at: row.get("paid_at"),
        payer_name: row.get("payer_name"),
        payer_bank: row.get("payer_bank"),
        reference_number: row.get("reference_number"),
        file_name: row.get("file_name").unwrap_or_default(),
        mime_type: row.get("mime_type").unwrap_or_default(),
        size_bytes: row.get("size_bytes").unwrap_or_default(),
        document_hash: row.get("document_hash").unwrap_or_default(),
        uploaded_by: row.get("uploaded_by"),
        uploaded_at: row.get("uploaded_at").unwrap_or_default(),
        reviewed_by: row.get("reviewed_by"),
        reviewed_at: row.get("reviewed_at"),
        review_note: row.get("review_note"),
    }
}

#[cfg(test)]
mod review_order_by_tests {
    use super::{
        manual_proof_upload_allowed, manual_review_allowed, offer_payment_slot_id, review_order_by,
        AcceptedOfferSnapshot, OFFER_PAYMENT_READY_STUDENT_STATUS,
    };

    #[test]
    fn absent_or_unknown_preserves_default() {
        assert_eq!(review_order_by("", ""), "ORDER BY activity_at ASC");
        // Injection attempt is not in the whitelist → default preserved.
        assert_eq!(
            review_order_by("p.x DESC //", "'; DROP"),
            "ORDER BY activity_at ASC"
        );
    }

    #[test]
    fn whitelisted_keys_apply_direction() {
        assert_eq!(review_order_by("parent", "asc"), "ORDER BY parent_name ASC");
        assert_eq!(review_order_by("due", "desc"), "ORDER BY amount DESC");
        assert_eq!(review_order_by("proof_age", "asc"), "ORDER BY age_days ASC");
        // Direction defaults to DESC when not "asc".
        assert_eq!(review_order_by("status", ""), "ORDER BY status DESC");
    }

    #[test]
    fn terminal_manual_states_cannot_accept_proof_or_review() {
        for status in ["paid", "expired", "failed", "cancelled"] {
            assert!(!manual_proof_upload_allowed(status));
            assert!(!manual_review_allowed(status));
        }
        assert!(manual_proof_upload_allowed("awaiting_proof"));
        assert!(manual_proof_upload_allowed("underpaid"));
        assert!(manual_review_allowed("pending_verification"));
    }

    #[test]
    fn payment_slot_is_bound_to_tenant_offer_revision_and_snapshot() {
        let mut offer = AcceptedOfferSnapshot {
            offer_id: "OFF-1".into(),
            offer_revision: 2,
            lead_id: "LEAD-1".into(),
            pricing_snapshot_hash: "hash-a".into(),
            pricing_snapshot_json: "{}".into(),
        };
        let original = offer_payment_slot_id("TENANT-1", &offer);
        assert_eq!(original, offer_payment_slot_id("TENANT-1", &offer));
        offer.offer_revision = 3;
        assert_ne!(original, offer_payment_slot_id("TENANT-1", &offer));
        offer.offer_revision = 2;
        assert_ne!(original, offer_payment_slot_id("TENANT-2", &offer));
        offer.pricing_snapshot_hash = "hash-b".into();
        assert_ne!(original, offer_payment_slot_id("TENANT-1", &offer));
    }

    #[test]
    fn accepted_offer_payment_waits_for_verified_documents() {
        assert_eq!(OFFER_PAYMENT_READY_STUDENT_STATUS, "documents_verified");
        let source = include_str!("payment_repository.rs");
        assert!(source.contains("applicantStatus,'') = 'offer_accepted'"));
        assert!(source.contains("request_type:'application_document_pack', status:'approved'"));
    }
}
