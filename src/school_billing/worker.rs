//! Issues whatever is due (monthly periods, installments) on a timer. Runs
//! only with `SCHOOL_BILLING_WORKER_ENABLED=true`; every issuance is
//! idempotent, so overlapping runs or several replicas cannot double-bill.
use super::{model::today, repository as repo, service};
use crate::school_invoice::repository::Error;
use std::{collections::BTreeSet, time::Duration};

pub fn enabled() -> bool {
    std::env::var("SCHOOL_BILLING_WORKER_ENABLED").as_deref() == Ok("true")
}

pub fn start(graph: std::sync::Arc<neo4rs::Graph>) {
    tokio::spawn(async move {
        loop {
            match run_once(&graph).await {
                Ok(issued) if issued > 0 => {
                    tracing::info!(issued, "school billing run issued invoices")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = ?e, "school billing run failed"),
            }
            tokio::time::sleep(Duration::from_secs(600)).await;
        }
    });
}

/// One pass over every child with a chosen period or a schedule.
pub async fn run_once(graph: &neo4rs::Graph) -> Result<usize, Error> {
    let mut pairs: BTreeSet<(String, String)> =
        repo::selections(graph).await?.into_iter().collect();
    for s in repo::all_schedules(graph).await? {
        pairs.insert((s.payer_user_id, s.student_id));
    }
    let today = today();
    let mut issued = 0;
    for (payer, student) in pairs {
        let child = match repo::family(graph, &payer, &student).await {
            Ok(c) => c,
            // Left the school or the family no longer passes the read: skip.
            Err(Error::Denied) => continue,
            Err(e) => return Err(e),
        };
        match service::issue_due(graph, &child, today).await {
            Ok(n) => issued += n,
            Err(e) => tracing::warn!(error = ?e, "school billing child skipped"),
        }
    }
    Ok(issued)
}
