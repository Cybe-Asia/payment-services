use neo4rs::{Graph, Query};

use crate::models::fee::FeeObligation;

/// Create (or find existing pending) FeeObligation for a lead × obligation_type.
/// Idempotent: if one already exists with status=pending, return it.
pub struct FeeObligationUpsert<'a> {
    pub tenant_id: &'a str,
    pub lead_id: &'a str,
    pub obligation_type: &'a str,
    pub amount_due: i64,
    pub currency: &'a str,
    pub due_iso: &'a str,
    pub new_id: &'a str,
    pub gross_amount: i64,
    pub discount_amount: i64,
    pub promotion_code: Option<&'a str>,
    pub promotion_rule_id: Option<&'a str>,
    pub promotion_snapshot_json: Option<&'a str>,
    pub line_items_json: &'a str,
}

pub async fn upsert_for_lead(
    graph: &Graph,
    input: FeeObligationUpsert<'_>,
) -> Result<FeeObligation, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         OPTIONAL MATCH (l)-[:OWES]->(existing:FeeObligation { \
            tenant_id:$tenant_id, obligation_type:$obligation_type, status:'pending' }) \
         WITH l, existing \
         FOREACH (_ IN CASE WHEN existing IS NULL THEN [1] ELSE [] END | \
            CREATE (l)-[:OWES]->(f:FeeObligation { \
              fee_obligation_id:$new_id, tenant_id:$tenant_id, obligation_type:$obligation_type, \
              amount_due:$amount_due, gross_amount:$gross_amount, discount_amount:$discount_amount, \
              net_amount:$amount_due, currency:$currency, status:'pending', \
              promotion_code:$promotion_code, promotion_rule_id:$promotion_rule_id, \
              promotion_snapshot_json:$promotion_snapshot_json, line_items_json:$line_items_json, \
              due_at:datetime($due_iso), created_at:datetime() \
            })) \
         WITH l \
         MATCH (l)-[:OWES]->(f:FeeObligation { \
            tenant_id:$tenant_id, obligation_type:$obligation_type, status:'pending' }) \
         SET f.amount_due = $amount_due, f.gross_amount = $gross_amount, \
             f.discount_amount = $discount_amount, f.net_amount = $amount_due, \
             f.currency = $currency, f.promotion_code = $promotion_code, \
             f.promotion_rule_id = $promotion_rule_id, \
             f.promotion_snapshot_json = $promotion_snapshot_json, \
             f.line_items_json = $line_items_json, f.updated_at = datetime() \
         RETURN f.fee_obligation_id AS id, f.tenant_id AS tenant_id, \
                f.obligation_type AS obligation_type, f.amount_due AS amount_due, \
                f.currency AS currency, f.status AS status, \
                toString(f.due_at) AS due_at \
         LIMIT 1".to_string(),
    )
    .param("lead_id", input.lead_id.to_string())
    .param("tenant_id", input.tenant_id.to_string())
    .param("obligation_type", input.obligation_type.to_string())
    .param("amount_due", input.amount_due)
    .param("currency", input.currency.to_string())
    .param("due_iso", input.due_iso.to_string())
    .param("new_id", input.new_id.to_string())
    .param("gross_amount", input.gross_amount)
    .param("discount_amount", input.discount_amount)
    .param(
        "promotion_code",
        input.promotion_code.unwrap_or("").to_string(),
    )
    .param(
        "promotion_rule_id",
        input.promotion_rule_id.unwrap_or("").to_string(),
    )
    .param(
        "promotion_snapshot_json",
        input.promotion_snapshot_json.unwrap_or("").to_string(),
    )
    .param("line_items_json", input.line_items_json.to_string());

    let mut result = graph.execute(q).await?;
    let row = result
        .next()
        .await?
        .ok_or_else(|| neo4rs::Error::UnknownMessage("no fee obligation returned".to_string()))?;

    Ok(FeeObligation {
        fee_obligation_id: row.get("id").unwrap_or_default(),
        tenant_id: row.get("tenant_id").unwrap_or_default(),
        obligation_type: row.get("obligation_type").unwrap_or_default(),
        amount_due: row.get::<i64>("amount_due").unwrap_or_default(),
        currency: row.get("currency").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        due_at: row.get("due_at"),
    })
}

#[allow(dead_code)]
pub async fn mark_settled(
    graph: &Graph,
    fee_obligation_id: &str,
    payment_id: &str,
) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (f:FeeObligation {fee_obligation_id:$fid}), (p:Payment {payment_id:$pid}) \
         SET f.status = 'paid', f.paid_at = datetime() \
         MERGE (f)-[:SETTLED_BY]->(p)"
            .to_string(),
    )
    .param("fid", fee_obligation_id.to_string())
    .param("pid", payment_id.to_string());
    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}
