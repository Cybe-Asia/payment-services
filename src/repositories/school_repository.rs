use neo4rs::{Graph, Query};

pub async fn find_school_id_by_code(
    graph: &Graph,
    tenant_id: &str,
    school_code: &str,
) -> Result<Option<String>, neo4rs::Error> {
    let raw_code = school_code.trim().to_uppercase();
    let canonical_code = canonical_school_code(&raw_code);
    let school_code_alias = format!("SCH-{canonical_code}");
    let school_id_alias = format!("SCHOOL-{canonical_code}");

    let q = Query::new(
        "MATCH (s:School {tenant_id:$tenant_id}) \
         WHERE s.school_code = $raw_code \
            OR s.school_code = $canonical_code \
            OR s.school_code = $school_code_alias \
            OR s.school_id = $raw_code \
            OR s.school_id = $school_id_alias \
         RETURN s.school_id AS id LIMIT 1"
            .to_string(),
    )
    .param("tenant_id", tenant_id.to_string())
    .param("raw_code", raw_code)
    .param("canonical_code", canonical_code)
    .param("school_code_alias", school_code_alias)
    .param("school_id_alias", school_id_alias);
    let mut result = graph.execute(q).await?;
    if let Some(row) = result.next().await? {
        Ok(row.get::<String>("id"))
    } else {
        Ok(None)
    }
}

fn canonical_school_code(school_code: &str) -> String {
    school_code
        .strip_prefix("SCH-")
        .or_else(|| school_code.strip_prefix("SCHOOL-"))
        .unwrap_or(school_code)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::canonical_school_code;

    #[test]
    fn canonical_school_code_accepts_prefixed_and_bare_codes() {
        assert_eq!(canonical_school_code("SCH-IIHS"), "IIHS");
        assert_eq!(canonical_school_code("SCHOOL-IISS"), "IISS");
        assert_eq!(canonical_school_code("IIHS"), "IIHS");
    }
}
