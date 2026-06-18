use neo4rs::{Graph, Query, Row};

use crate::models::payment::Payment;
use crate::models::payment_proof::PaymentProof;

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
            status:'pending', amount:$amount, currency:$currency, \
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
            status:'awaiting_proof', amount:$amount, currency:$currency, \
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
    find_by(graph, "p.payment_id = $val", payment_id).await
}

#[allow(dead_code)]
pub async fn find_by_gateway_ref(
    graph: &Graph,
    gateway_ref: &str,
) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.gateway_ref = $val", gateway_ref).await
}

async fn find_by(
    graph: &Graph,
    predicate: &str,
    val: &str,
) -> Result<Option<Payment>, neo4rs::Error> {
    let cypher = format!(
        "MATCH (p:Payment) WHERE {predicate} \
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
    let q = Query::new(cypher).param("val", val.to_string());
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
         SET p.status = 'paid', p.paid_at = datetime(), \
             p.payment_method = coalesce($method, p.payment_method), \
             p.receipt_ref = coalesce($receipt, p.receipt_ref) \
         WITH p \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         SET l.status = 'paid' \
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
        "MATCH (p:Payment {payment_id:$payment_id}) \
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
) -> Result<Option<(String, String, Option<String>)>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment)-[:HAS_PROOF]->(proof:PaymentProof {payment_proof_id:$proof_id}) \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         RETURN proof.object_key AS object_key, proof.file_name AS file_name, l.lead_id AS lead_id \
         LIMIT 1"
            .to_string(),
    )
    .param("proof_id", proof_id.to_string());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some((
            row.get("object_key").unwrap_or_default(),
            row.get("file_name").unwrap_or_default(),
            row.get("lead_id"),
        )))
    } else {
        Ok(None)
    }
}

pub async fn list_review_rows(
    graph: &Graph,
    status: &str,
    school: &str,
    search: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<PaymentReviewRow>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead)-[:MADE_PAYMENT]->(p:Payment) \
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
         RETURN p.payment_id AS payment_id, l.lead_id AS lead_id, \
                l.parent_name AS parent_name, l.email AS parent_email, \
                l.target_school_preference AS school, p.payment_type AS payment_type, \
                p.status AS status, p.amount AS amount, p.currency AS currency, \
                p.amount_submitted AS amount_submitted, p.amount_verified AS amount_verified, \
                p.short_amount AS short_amount, proof.payment_proof_id AS latest_proof_id, \
                proof.file_name AS latest_proof_file_name, proof.amount_submitted AS latest_proof_amount, \
                toString(proof.uploaded_at) AS latest_proof_uploaded_at, \
                duration.inDays(coalesce(proof.uploaded_at, p.created_at), datetime()).days AS age_days \
         ORDER BY coalesce(proof.uploaded_at, p.created_at) ASC \
         SKIP $offset LIMIT $limit".to_string(),
    )
    .param("status", status.to_string())
    .param("school", school.to_string())
    .param("search", search.to_string())
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
            age_days: row.get("age_days"),
        });
    }
    Ok(rows)
}

pub async fn count_review_rows(
    graph: &Graph,
    status: &str,
    school: &str,
    search: &str,
) -> Result<i64, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead)-[:MADE_PAYMENT]->(p:Payment) \
         WHERE p.payment_method = 'manual_transfer' \
           AND ($status = '' OR p.status = $status) \
           AND ($school = '' OR toLower(coalesce(l.target_school_preference, '')) = toLower($school)) \
           AND ( \
             $search = '' OR \
             toLower(coalesce(l.parent_name, '')) CONTAINS toLower($search) OR \
             toLower(coalesce(l.email, '')) CONTAINS toLower($search) OR \
             toLower(p.payment_id) CONTAINS toLower($search) \
           ) \
         RETURN count(p) AS total".to_string(),
    )
    .param("status", status.to_string())
    .param("school", school.to_string())
    .param("search", search.to_string());
    let mut result = graph.execute(q).await?;
    Ok(result
        .next()
        .await?
        .and_then(|row| row.get::<i64>("total"))
        .unwrap_or(0))
}

pub async fn find_review_detail(
    graph: &Graph,
    payment_id: &str,
) -> Result<Option<PaymentReviewDetail>, neo4rs::Error> {
    let payment = match find_by_id(graph, payment_id).await? {
        Some(payment) => payment,
        None => return Ok(None),
    };

    let lead_id = payment.lead_id.clone().unwrap_or_default();
    let lead = fetch_review_lead(graph, payment_id)
        .await?
        .unwrap_or(PaymentReviewLead {
            lead_id,
            parent_name: String::new(),
            parent_email: String::new(),
            school: String::new(),
        });
    let proofs = list_proofs_for_payment(graph, payment_id).await?;

    Ok(Some(PaymentReviewDetail {
        payment,
        lead,
        proofs,
    }))
}

pub struct ManualPaymentReviewUpdate<'a> {
    pub payment_id: &'a str,
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
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id}) \
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
             SET p.paid_at = datetime() \
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
    let _ = result.next().await?;
    Ok(())
}

async fn fetch_review_lead(
    graph: &Graph,
    payment_id: &str,
) -> Result<Option<PaymentReviewLead>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead)-[:MADE_PAYMENT]->(:Payment {payment_id:$payment_id}) \
         RETURN l.lead_id AS lead_id, l.parent_name AS parent_name, \
                l.email AS parent_email, l.target_school_preference AS school \
         LIMIT 1"
            .to_string(),
    )
    .param("payment_id", payment_id.to_string());
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
    payment_id: &str,
) -> Result<Vec<PaymentProof>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id})-[:HAS_PROOF]->(proof:PaymentProof) \
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
    .param("payment_id", payment_id.to_string());
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
