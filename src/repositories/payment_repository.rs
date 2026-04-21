use neo4rs::{Graph, Query};

use crate::models::payment::Payment;

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
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}), (f:FeeObligation {fee_obligation_id:$fid}) \
         CREATE (p:Payment { \
            payment_id:$payment_id, tenant_id:$tenant_id, payment_type:$payment_type, \
            status:'pending', amount:$amount, currency:$currency, \
            gateway_ref:$gateway_ref, invoice_ref:$invoice_ref, \
            hosted_invoice_url:$hosted_invoice_url, \
            expires_at:datetime($expires_iso), created_at:datetime() \
         }) \
         MERGE (l)-[:MADE_PAYMENT]->(p) \
         MERGE (f)-[:SETTLED_BY]->(p) \
         RETURN p".to_string(),
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
    .param("lead_id", lead_id.to_string());

    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

pub async fn find_by_id(graph: &Graph, payment_id: &str) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.payment_id = $val", payment_id).await
}

pub async fn find_by_gateway_ref(graph: &Graph, gateway_ref: &str) -> Result<Option<Payment>, neo4rs::Error> {
    find_by(graph, "p.gateway_ref = $val", gateway_ref).await
}

async fn find_by(graph: &Graph, predicate: &str, val: &str) -> Result<Option<Payment>, neo4rs::Error> {
    let cypher = format!(
        "MATCH (p:Payment) WHERE {predicate} \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         RETURN p.payment_id AS payment_id, p.tenant_id AS tenant_id, \
                p.payment_type AS payment_type, p.status AS status, \
                p.amount AS amount, p.currency AS currency, \
                p.payment_method AS payment_method, p.gateway_ref AS gateway_ref, \
                p.invoice_ref AS invoice_ref, p.hosted_invoice_url AS hosted_invoice_url, \
                p.receipt_ref AS receipt_ref, \
                toString(p.paid_at) AS paid_at, toString(p.expires_at) AS expires_at, \
                l.lead_id AS lead_id \
         LIMIT 1"
    );
    let q = Query::new(cypher).param("val", val.to_string());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some(Payment {
            payment_id: row.get("payment_id").unwrap_or_default(),
            tenant_id: row.get("tenant_id").unwrap_or_default(),
            payment_type: row.get("payment_type").unwrap_or_default(),
            status: row.get("status").unwrap_or_default(),
            amount: row.get::<i64>("amount").unwrap_or_default(),
            currency: row.get("currency").unwrap_or_default(),
            payment_method: row.get("payment_method"),
            gateway_ref: row.get("gateway_ref"),
            invoice_ref: row.get("invoice_ref"),
            hosted_invoice_url: row.get("hosted_invoice_url"),
            receipt_ref: row.get("receipt_ref"),
            paid_at: row.get("paid_at"),
            expires_at: row.get("expires_at"),
            lead_id: row.get("lead_id"),
        }))
    } else {
        Ok(None)
    }
}

pub async fn mark_paid(graph: &Graph, payment_id: &str, payment_method: Option<&str>, receipt_ref: Option<&str>) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id}) \
         SET p.status = 'paid', p.paid_at = datetime(), \
             p.payment_method = coalesce($method, p.payment_method), \
             p.receipt_ref = coalesce($receipt, p.receipt_ref) \
         WITH p \
         OPTIONAL MATCH (l:Lead)-[:MADE_PAYMENT]->(p) \
         SET l.status = 'paid' \
         RETURN p".to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("method", payment_method.map(|s| s.to_string()).unwrap_or_default())
    .param("receipt", receipt_ref.map(|s| s.to_string()).unwrap_or_default());
    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}

pub async fn mark_status(graph: &Graph, payment_id: &str, status: &str) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (p:Payment {payment_id:$payment_id}) SET p.status = $status RETURN p".to_string(),
    )
    .param("payment_id", payment_id.to_string())
    .param("status", status.to_string());
    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}
