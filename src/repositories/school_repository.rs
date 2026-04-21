use neo4rs::{Graph, Query};

pub async fn find_school_id_by_code(graph: &Graph, tenant_id: &str, school_code: &str) -> Result<Option<String>, neo4rs::Error> {
    let q = Query::new(
        "MATCH (s:School {tenant_id:$tenant_id, school_code:$code}) RETURN s.school_id AS id LIMIT 1".to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("code", school_code.to_string());
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(row.get::<String>("id"))
    } else {
        Ok(None)
    }
}
