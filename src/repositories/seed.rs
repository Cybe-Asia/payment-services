use neo4rs::{Graph, Query};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

/// Schools the service creates when bootstrap is on. IIBS is not one of them.
const SEED_SCHOOLS: [(&str, &str); 2] = [
    ("IIHS", "International Islamic High School"),
    ("IISS", "International Islamic Secondary School"),
];

fn seed_school_id(code: &str) -> String {
    format!("SCH-{code}")
}

/// Seed Tenant + Schools + initial FeeStructure nodes. Idempotent: uses MERGE
/// so re-running the service does not duplicate or clobber amounts once an
/// admin has edited them via the admin endpoint.
///
/// Retries on transient connection errors — neo4rs lazy-initialises the
/// connection pool, so the very first query during pod startup can race
/// with the neo4j service DNS/endpoints becoming ready.
pub async fn seed_fees(graph: &Graph, tenant_id: &str) -> Result<(), String> {
    // 1) Tenant (with retry — this is the first query; absorbs startup race)
    let tenant_q = || {
        Query::new(
            "MERGE (t:Tenant {tenant_id: $tid}) \
             ON CREATE SET t.tenant_code = 'cybe-asia', \
                           t.tenant_name = 'Cybe Asia', \
                           t.status = 'active', \
                           t.created_at = datetime() \
             RETURN t"
                .to_string(),
        )
        .param("tid", tenant_id.to_string())
    };
    let mut last_err: Option<String> = None;
    for attempt in 1..=8 {
        match graph.run(tenant_q()).await {
            Ok(_) => {
                last_err = None;
                break;
            }
            Err(e) => {
                last_err = Some(format!("attempt {attempt}: {e}"));
                warn!("seed Tenant retry {attempt}: {e}");
                sleep(Duration::from_millis(1500)).await;
            }
        }
    }
    if let Some(e) = last_err {
        return Err(format!("seed Tenant failed after retries: {e}"));
    }

    // 2) Schools. The identity is `school_id` (`SCH-<code>`). A School that
    //    already answers to the id or the code is left alone, so a re-run never
    //    adds a second node for a code (a bare MERGE on school_code did, and
    //    the lookup then returned an arbitrary one of the two).
    for (code, name) in SEED_SCHOOLS {
        let school_id = seed_school_id(code);
        let exists = Query::new(
            "MATCH (s:School {tenant_id:$tid}) \
             WHERE s.school_id = $sid OR s.school_code = $code \
             RETURN count(s) AS n"
                .to_string(),
        )
        .param("tid", tenant_id.to_string())
        .param("sid", school_id.clone())
        .param("code", code.to_string());
        let mut r = graph
            .execute(exists)
            .await
            .map_err(|e| format!("seed School {code} lookup failed: {e}"))?;
        let present = match r.next().await {
            Ok(Some(row)) => row.get::<i64>("n").unwrap_or(0),
            Ok(None) => 0,
            Err(e) => return Err(format!("seed School {code} lookup failed: {e}")),
        };
        if present > 0 {
            if present > 1 {
                warn!(
                    "seed School {code}: {present} School nodes already match; not touching them"
                );
            }
            continue;
        }
        let q = Query::new(
            "MERGE (s:School {tenant_id:$tid, school_id:$sid}) \
             ON CREATE SET s.school_code = $code, \
                           s.school_name = $name, \
                           s.school_type = 'secondary_islamic', \
                           s.status = 'active', \
                           s.created_at = datetime() \
             RETURN s"
                .to_string(),
        )
        .param("tid", tenant_id.to_string())
        .param("sid", school_id)
        .param("code", code.to_string())
        .param("name", name.to_string());
        graph
            .run(q)
            .await
            .map_err(|e| format!("seed School {code} failed: {e}"))?;
    }

    // 3) FeeStructure — only seed if no active structure exists yet. Admin
    //    edits via supersede create a new active row, we never overwrite.
    for (code, _) in SEED_SCHOOLS {
        let fs_id = format!("FEE-{code}-APPFEE-V1");
        let q = Query::new(
            "MATCH (s:School {tenant_id:$tid, school_id:$sid}) \
             OPTIONAL MATCH (s)-[:HAS_FEE_STRUCTURE]->(existing:FeeStructure { \
                tenant_id:$tid, payment_type:'application_fee', status:'active' }) \
             WITH s, existing \
             FOREACH (_ IN CASE WHEN existing IS NULL THEN [1] ELSE [] END | \
                CREATE (s)-[:HAS_FEE_STRUCTURE]->(fs:FeeStructure { \
                  fee_structure_id:$fs_id, tenant_id:$tid, school_id:s.school_id, \
                  payment_type:'application_fee', amount:2200000, currency:'IDR', \
                  status:'active', effective_from:datetime(), effective_to:null })) \
             RETURN s.school_code AS c"
                .to_string(),
        )
        .param("tid", tenant_id.to_string())
        .param("sid", seed_school_id(code))
        .param("fs_id", fs_id);
        match graph.execute(q).await {
            Ok(mut r) => {
                let _ = r.next().await;
            }
            Err(e) => warn!("seed FeeStructure {code} warn: {e}"),
        }
    }

    info!("seed complete for tenant {}", tenant_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{seed_school_id, SEED_SCHOOLS};

    #[test]
    fn seed_ids_are_sch_prefixed_and_unique() {
        let ids: Vec<String> = SEED_SCHOOLS
            .iter()
            .map(|(c, _)| seed_school_id(c))
            .collect();
        assert_eq!(ids, ["SCH-IIHS", "SCH-IISS"]);
    }

    #[test]
    fn iibs_is_not_seeded() {
        assert!(SEED_SCHOOLS.iter().all(|(c, _)| *c != "IIBS"));
    }
}
