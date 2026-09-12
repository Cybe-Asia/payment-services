use neo4rs::{query, Graph};
const MAX_ATTEMPTS: i64 = 5;
pub async fn claim(
    graph: &Graph,
    tenant: &str,
    id: &str,
    nonce: &str,
) -> Result<Option<String>, neo4rs::Error> {
    let mut rows = graph.execute(query(
        "MATCH (l:Lead {tenant_id:$tenant})-[:MADE_PAYMENT]->(p:Payment {tenant_id:$tenant,payment_id:$id})
         SET p.receipt_email_lock=coalesce(p.receipt_email_lock,0)+1
         WITH l,p WHERE p.receipt_email_status IN ['queued','retry','sending']
           AND coalesce(p.receipt_email_attempts,0)<$max
           AND (p.receipt_email_next_at IS NULL OR p.receipt_email_next_at<=datetime())
           AND p.status='paid' AND p.payment_type='application_fee'
         SET p.receipt_email_status='sending',p.receipt_email_nonce=$nonce,
             p.receipt_email_attempts=coalesce(p.receipt_email_attempts,0)+1,
             p.receipt_email_next_at=datetime()+duration({seconds:120})
         RETURN l.email AS email")
        .param("tenant",tenant).param("id",id).param("nonce",nonce).param("max",MAX_ATTEMPTS)).await?;
    Ok(rows
        .next()
        .await?
        .map(|row| row.get::<String>("email").unwrap_or_default()))
}

pub async fn complete(
    graph: &Graph,
    tenant: &str,
    id: &str,
    nonce: &str,
    sent: bool,
) -> Result<(), neo4rs::Error> {
    graph.run(query(
        "MATCH (p:Payment {tenant_id:$tenant,payment_id:$id,receipt_email_nonce:$nonce})
         SET p.receipt_email_status=CASE WHEN $sent THEN 'sent' WHEN p.receipt_email_attempts >= $max THEN 'failed' ELSE 'retry' END,
             p.receipt_email_next_at=datetime()+duration({seconds:300}),p.receipt_email_updated_at=datetime()")
        .param("tenant",tenant).param("id",id).param("nonce",nonce).param("sent",sent).param("max",MAX_ATTEMPTS)).await
}

pub async fn pending_ids(graph: &Graph, tenant: &str) -> Result<Vec<String>, neo4rs::Error> {
    graph.run(query("MATCH (p:Payment {tenant_id:$tenant,receipt_email_status:'sending'}) WHERE p.receipt_email_attempts >= $max AND p.receipt_email_next_at<=datetime() SET p.receipt_email_status='failed',p.receipt_email_updated_at=datetime()")
        .param("tenant",tenant.to_string()).param("max",MAX_ATTEMPTS)).await?;
    let mut rows = graph.execute(query(
        "MATCH (p:Payment {tenant_id:$tenant}) WHERE p.receipt_email_status IN ['queued','retry','sending']
         AND coalesce(p.receipt_email_attempts,0)<$max
         AND (p.receipt_email_next_at IS NULL OR p.receipt_email_next_at<=datetime())
         AND p.status='paid' AND p.payment_type='application_fee'
         RETURN p.payment_id AS id LIMIT 20")
        .param("tenant",tenant.to_string()).param("max",MAX_ATTEMPTS)).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Some(id) = row.get::<String>("id") {
            ids.push(id);
        }
    }
    Ok(ids)
}
