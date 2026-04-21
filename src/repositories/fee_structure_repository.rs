use neo4rs::{Graph, Query};

use crate::models::fee::FeeStructure;

/// Return the active FeeStructure for a given school × payment_type.
/// "Active" = status=active AND effective_to is null OR in the future.
pub async fn find_active(graph: &Graph, tenant_id: &str, school_id: &str, payment_type: &str) -> Result<Option<FeeStructure>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (s:School {school_id:$school_id})-[:HAS_FEE_STRUCTURE]->(fs:FeeStructure { \
            tenant_id:$tenant_id, payment_type:$payment_type, status:'active' }) \
         WHERE fs.effective_to IS NULL OR fs.effective_to > datetime() \
         RETURN fs.fee_structure_id AS fee_structure_id, \
                fs.tenant_id AS tenant_id, \
                fs.school_id AS school_id, \
                s.school_code AS school_code, \
                fs.payment_type AS payment_type, \
                fs.amount AS amount, \
                fs.currency AS currency, \
                fs.status AS status \
         ORDER BY fs.effective_from DESC LIMIT 1".to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("school_id", school_id.to_string())
    .param("payment_type", payment_type.to_string());

    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(Some(FeeStructure {
            fee_structure_id: row.get("fee_structure_id").unwrap_or_default(),
            tenant_id: row.get("tenant_id").unwrap_or_default(),
            school_id: row.get("school_id").unwrap_or_default(),
            school_code: row.get("school_code").unwrap_or_default(),
            payment_type: row.get("payment_type").unwrap_or_default(),
            amount: row.get::<i64>("amount").unwrap_or_default(),
            currency: row.get("currency").unwrap_or_default(),
            status: row.get("status").unwrap_or_default(),
        }))
    } else {
        Ok(None)
    }
}

/// Update amount on the active FeeStructure for (school, payment_type). Admin
/// operation — supersedes the previous version with effective_to=now and
/// creates a new active row.
pub async fn supersede_amount(graph: &Graph, tenant_id: &str, school_id: &str, payment_type: &str, new_amount: i64, currency: &str, new_id: &str) -> Result<(), neo4rs::Error> {
    let q = Query::new(
        "MATCH (s:School {school_id:$school_id})-[:HAS_FEE_STRUCTURE]->(old:FeeStructure { \
            tenant_id:$tenant_id, payment_type:$payment_type, status:'active' }) \
         SET old.status = 'superseded', old.effective_to = datetime() \
         WITH s \
         CREATE (s)-[:HAS_FEE_STRUCTURE]->(fs:FeeStructure { \
            fee_structure_id:$new_id, tenant_id:$tenant_id, school_id:$school_id, \
            payment_type:$payment_type, amount:$amount, currency:$currency, \
            status:'active', effective_from: datetime(), effective_to: null }) \
         RETURN fs".to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("school_id", school_id.to_string())
    .param("payment_type", payment_type.to_string())
    .param("amount", new_amount)
    .param("currency", currency.to_string())
    .param("new_id", new_id.to_string());

    let mut result = graph.execute(q).await?;
    let _ = result.next().await?;
    Ok(())
}
