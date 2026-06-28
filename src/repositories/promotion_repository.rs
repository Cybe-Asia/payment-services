use neo4rs::{Graph, Query};

#[derive(Clone, Debug)]
pub struct PromotionRuleSnapshot {
    pub promotion_code: String,
    pub promotion_rule_id: String,
    pub source: String,
    pub discount_type: String,
    pub discount_value: i64,
    pub max_discount_amount: Option<i64>,
    pub min_net_amount: Option<i64>,
}

pub async fn find_active_for_lead(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    if let Some(explicit) = find_explicit_lead_promotion(graph, lead_id, payment_type).await? {
        return Ok(Some(explicit));
    }
    find_reference_code_promotion(graph, lead_id, payment_type).await
}

async fn find_explicit_lead_promotion(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         WHERE coalesce(l.promotion_code, '') <> '' \
         MATCH (p:PromotionCode {normalized_code:l.promotion_code}) \
         WHERE p.status IN ['active', 'approved'] \
           AND p.payment_type_scope = $payment_type \
           AND coalesce(p.approved_by, '') <> '' \
         RETURN p.normalized_code AS promotionCode, \
                p.promotion_code_id AS promotionRuleId, \
                p.discount_type AS discountType, \
                p.discount_value AS discountValue, \
                p.max_discount_amount AS maxDiscountAmount, \
                p.min_net_amount AS minNetAmount \
         ORDER BY p.approved_at DESC \
         LIMIT 1"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        return Ok(Some(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: "lead_promotion_code".to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        }));
    }

    Ok(None)
}

async fn find_reference_code_promotion(
    graph: &Graph,
    lead_id: &str,
    payment_type: &str,
) -> Result<Option<PromotionRuleSnapshot>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id}) \
         WHERE coalesce(l.reference_code_status, '') = 'verified' \
         WITH coalesce(l.reference_code, l.referral_code, '') AS code \
         MATCH (r:ReferenceCode {normalized_code: code})-[:HAS_PROMOTION_RULE]->(p:PromotionRule) \
         WHERE r.status = 'active' \
           AND p.status = 'active' \
           AND p.payment_type_scope = $payment_type \
           AND coalesce(p.approved_by, '') <> '' \
         RETURN r.normalized_code AS promotionCode, \
                p.promotion_rule_id AS promotionRuleId, \
                p.discount_type AS discountType, \
                p.discount_value AS discountValue, \
                p.max_discount_amount AS maxDiscountAmount, \
                p.min_net_amount AS minNetAmount \
         ORDER BY p.approved_at DESC \
         LIMIT 1"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        return Ok(Some(PromotionRuleSnapshot {
            promotion_code: row.get("promotionCode").unwrap_or_default(),
            promotion_rule_id: row.get("promotionRuleId").unwrap_or_default(),
            source: "reference_code".to_string(),
            discount_type: row.get("discountType").unwrap_or_default(),
            discount_value: row.get::<i64>("discountValue").unwrap_or_default(),
            max_discount_amount: row.get("maxDiscountAmount"),
            min_net_amount: row.get("minNetAmount"),
        }));
    }

    Ok(None)
}
