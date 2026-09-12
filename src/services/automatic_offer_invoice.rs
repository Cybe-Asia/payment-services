//! Creates exactly one manual-transfer invoice from a newly accepted immutable offer.
//! It never charges a gateway or infers a bank account.
use crate::{services::payment_service, AppState};
use neo4rs::{query, Graph};
struct Work {
    offer: String,
    lead: String,
    bank: String,
    nonce: String,
    attempts: i64,
}
async fn claim(graph: &Graph, tenant: &str) -> Result<Option<Work>, neo4rs::Error> {
    graph.run(query("MATCH (o:Offer {tenant_id:$tenant,auto_generated:true,invoice_creation_status:'sending'}) WHERE o.invoice_creation_attempts>=5 AND o.invoice_creation_next<=datetime() SET o.invoice_creation_status='failed' REMOVE o.invoice_creation_nonce").param("tenant",tenant)).await?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut rows=graph.execute(query("MATCH (l:Lead {tenant_id:$tenant})-[:HAS_STUDENT]->(:Student)-[:HAS_OFFER]->(o:Offer {tenant_id:$tenant,auto_generated:true,status:'accepted'}) WHERE o.invoice_creation_status IN ['queued','retry','sending'] AND coalesce(o.invoice_creation_attempts,0)<5 AND (o.invoice_creation_next IS NULL OR o.invoice_creation_next<=datetime()) WITH l,o LIMIT 1 SET o.invoice_creation_lock=coalesce(o.invoice_creation_lock,0)+1 WITH l,o WHERE o.invoice_creation_status IN ['queued','retry','sending'] AND coalesce(o.invoice_creation_attempts,0)<5 AND (o.invoice_creation_next IS NULL OR o.invoice_creation_next<=datetime()) SET o.invoice_creation_status='sending',o.invoice_creation_nonce=$nonce,o.invoice_creation_next=datetime()+duration('PT120S'),o.invoice_creation_attempts=coalesce(o.invoice_creation_attempts,0)+1 RETURN l.lead_id AS lead,o.offer_id AS offer,o.bank_account_id AS bank,o.invoice_creation_attempts AS attempts").param("tenant",tenant).param("nonce",nonce.clone())).await?;
    Ok(rows.next().await?.map(|r| Work {
        offer: r.get("offer").unwrap_or_default(),
        lead: r.get("lead").unwrap_or_default(),
        bank: r.get("bank").unwrap_or_default(),
        attempts: r.get("attempts").unwrap_or(1),
        nonce,
    }))
}
pub fn start_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Some(graph) = state.graph.as_ref() {
                for _ in 0..20 {
                    let w = match claim(graph, &state.tenant_id).await {
                        Ok(Some(w)) => w,
                        _ => break,
                    };
                    let ok = !w.bank.is_empty()
                        && payment_service::create_offer_manual_payment(
                            graph,
                            &state.tenant_id,
                            &w.offer,
                            &[w.lead.clone()],
                            Some(&w.bank),
                            state.default_due_hours,
                            &state.payment_settings_seed,
                        )
                        .await
                        .is_ok();
                    let status = if ok {
                        "created"
                    } else if w.attempts >= 5 {
                        "failed"
                    } else {
                        "retry"
                    };
                    let _=graph.run(query("MATCH (o:Offer {tenant_id:$tenant,offer_id:$offer,invoice_creation_nonce:$nonce}) SET o.invoice_creation_status=$status,o.invoice_creation_next=datetime()+duration('PT60S') REMOVE o.invoice_creation_nonce").param("tenant",state.tenant_id.clone()).param("offer",w.offer).param("nonce",w.nonce).param("status",status)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::{
        payment_repository, payment_settings_repository::PaymentSettingsSeed,
    };
    use sha2::{Digest, Sha256};
    #[tokio::test]
    #[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
    async fn automatic_invoice_uses_accepted_amount_bank_and_single_slot() {
        let graph = Graph::new(
            &std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap(),
            "neo4j",
            "test",
        )
        .await
        .unwrap();
        payment_repository::init_doku_indexes(&graph).await.unwrap();
        let id = format!("auto-invoice-test-{}", uuid::Uuid::new_v4());
        let json =
            r#"{"snapshotVersion":"offer-pricing-v1","currency":"IDR","amountDueNow":9000000}"#;
        let hash = hex::encode(Sha256::digest(json.as_bytes()));
        graph.run(query("CREATE (l:Lead {tenant_id:$id,lead_id:$id,email:'parent@example.invalid'})-[:HAS_STUDENT]->(s:Student {studentId:$id,applicantStatus:'offer_accepted'}) CREATE (s)-[:REQUIRES_DOCUMENT]->(:DocumentRequest {request_type:'application_document_pack',status:'approved'}) CREATE (s)-[:HAS_OFFER]->(o:Offer {offer_id:$id,tenant_id:$id,status:'accepted',auto_generated:true,bank_account_id:'BANK-A',invoice_creation_status:'queued',revision:1,pricing_snapshot_json:$json,pricing_snapshot_hash:$hash,terms_hash:'terms'}) CREATE (o)-[:ACCEPTED_VIA]->(:OfferAcceptance {status:'accepted',offer_revision:1,pricing_snapshot_hash:$hash,terms_hash:'terms'}) CREATE (:PaymentSettings {tenant_id:$id,manual_transfer_enabled:true,manual_bank_accounts_json:$banks})").param("id",id.clone()).param("json",json).param("hash",hash).param("banks",r#"[{"id":"BANK-A","bankName":"Test Bank","accountName":"Test School","accountNumber":"12345","enabled":true,"instructions":"Synthetic only"}]"#)).await.unwrap();
        assert!(claim(&graph, "other-tenant").await.unwrap().is_none());
        let w = claim(&graph, &id).await.unwrap().unwrap();
        assert_eq!(w.bank, "BANK-A");
        assert!(claim(&graph, &id).await.unwrap().is_none());
        let seed = PaymentSettingsSeed {
            tenant_id: id.clone(),
            bank_name: String::new(),
            bank_account_name: String::new(),
            bank_account_number: String::new(),
            instructions: String::new(),
        };
        assert!(payment_service::create_offer_manual_payment(
            &graph,
            &id,
            &id,
            &["other-parent".into()],
            Some("BANK-A"),
            48,
            &seed
        )
        .await
        .is_err());
        assert!(payment_service::create_offer_manual_payment(
            &graph,
            &id,
            &id,
            &[id.clone()],
            Some("BANK-B"),
            48,
            &seed
        )
        .await
        .is_err());
        let payment = payment_service::create_offer_manual_payment(
            &graph,
            &id,
            &id,
            &[id.clone()],
            Some("BANK-A"),
            48,
            &seed,
        )
        .await
        .unwrap()
        .payment;
        assert_eq!(payment.amount, 9_000_000);
        let repeated = payment_service::create_offer_manual_payment(
            &graph,
            &id,
            &id,
            &[id.clone()],
            None,
            48,
            &seed,
        )
        .await
        .unwrap()
        .payment;
        assert_eq!(payment.payment_id, repeated.payment_id);
        let mut rows=graph.execute(query("MATCH (o:Offer {offer_id:$id})-[:PAID_VIA]->(p) RETURN count(p) AS count,collect(p.invoice_email_status) AS emails,collect(p.manual_bank_account_id) AS banks").param("id",id.clone())).await.unwrap();
        let r = rows.next().await.unwrap().unwrap();
        assert_eq!(r.get::<i64>("count").unwrap(), 1);
        assert_eq!(r.get::<Vec<String>>("emails").unwrap(), vec!["queued"]);
        assert_eq!(r.get::<Vec<String>>("banks").unwrap(), vec!["BANK-A"]);
        graph
            .run(query("MATCH (n {tenant_id:$id}) DETACH DELETE n").param("id", id))
            .await
            .unwrap();
    }
}
