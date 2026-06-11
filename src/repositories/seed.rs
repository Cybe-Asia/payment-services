use neo4rs::{Graph, Query};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

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

    // 2) Schools
    for (code, name) in [
        ("IIHS", "International Islamic High School"),
        ("IISS", "International Islamic Secondary School"),
        ("IIBS", "International Islamic Boarding School"),
    ] {
        let q = Query::new(
            "MERGE (s:School {tenant_id:$tid, school_code:$code}) \
             ON CREATE SET s.school_id = 'SCHOOL-' + $code, \
                           s.school_name = $name, \
                           s.school_type = 'secondary_islamic', \
                           s.status = 'active', \
                           s.created_at = datetime() \
             RETURN s"
                .to_string(),
        )
        .param("tid", tenant_id.to_string())
        .param("code", code.to_string())
        .param("name", name.to_string());
        graph
            .run(q)
            .await
            .map_err(|e| format!("seed School {code} failed: {e}"))?;
    }

    // 3) FeeStructure — only seed if no active structure exists yet. Admin
    //    edits via supersede create a new active row, we never overwrite.
    for code in ["IIHS", "IISS", "IIBS"] {
        let fs_id = format!("FEE-{code}-APPFEE-V1");
        let q = Query::new(
            "MATCH (s:School {tenant_id:$tid, school_code:$code}) \
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
        .param("code", code.to_string())
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
