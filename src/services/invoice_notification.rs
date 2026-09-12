//! Durable invoice email delivery. Only newly created, explicitly queued invoices
//! are eligible; pre-existing invoices are never backfilled.
use crate::models::payment::Payment;
use crate::{repositories::payment_repository, AppState};
use neo4rs::{query, Graph};

const MAX_ATTEMPTS: i64 = 5;

pub fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("invoice HTTP client");
        loop {
            if let Some(graph) = state.graph.as_ref() {
                let _ = deliver_batch(graph, &state, &client).await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}

async fn claim(
    graph: &Graph,
    tenant: &str,
    id: &str,
    nonce: &str,
) -> Result<Option<String>, neo4rs::Error> {
    let mut rows = graph.execute(query(
        "MATCH (l:Lead {tenant_id:$tenant})-[:MADE_PAYMENT]->(p:Payment {tenant_id:$tenant,payment_id:$id})
         SET p.invoice_email_lock=coalesce(p.invoice_email_lock,0)+1
         WITH l,p WHERE p.invoice_email_status IN ['queued','retry','sending']
           AND coalesce(p.invoice_email_attempts,0)<$max
           AND (p.invoice_email_next_at IS NULL OR p.invoice_email_next_at<=datetime())
           AND p.status IN ['pending','awaiting_proof'] AND (p.expires_at IS NULL OR p.expires_at>datetime())
         SET p.invoice_email_status='sending',p.invoice_email_nonce=$nonce,
             p.invoice_email_attempts=coalesce(p.invoice_email_attempts,0)+1,
             p.invoice_email_next_at=datetime()+duration({seconds:120})
         RETURN l.email AS email")
        .param("tenant",tenant).param("id",id).param("nonce",nonce).param("max",MAX_ATTEMPTS)).await?;
    Ok(rows
        .next()
        .await?
        .map(|row| row.get::<String>("email").unwrap_or_default()))
}

async fn complete(
    graph: &Graph,
    tenant: &str,
    id: &str,
    nonce: &str,
    sent: bool,
) -> Result<(), neo4rs::Error> {
    graph.run(query(
        "MATCH (p:Payment {tenant_id:$tenant,payment_id:$id,invoice_email_nonce:$nonce})
         SET p.invoice_email_status=CASE WHEN $sent THEN 'sent' WHEN p.invoice_email_attempts >= $max THEN 'failed' ELSE 'retry' END,
             p.invoice_email_next_at=datetime()+duration({seconds:300}),p.invoice_email_updated_at=datetime()")
        .param("tenant",tenant).param("id",id).param("nonce",nonce).param("sent",sent).param("max",MAX_ATTEMPTS)).await
}

async fn deliver_batch(
    graph: &Graph,
    state: &AppState,
    client: &reqwest::Client,
) -> Result<(), neo4rs::Error> {
    graph.run(query("MATCH (p:Payment {tenant_id:$tenant,invoice_email_status:'sending'}) WHERE p.invoice_email_attempts >= $max AND p.invoice_email_next_at<=datetime() SET p.invoice_email_status='failed',p.invoice_email_updated_at=datetime()")
        .param("tenant",state.tenant_id.clone()).param("max",MAX_ATTEMPTS)).await?;
    let mut rows = graph.execute(query(
        "MATCH (p:Payment {tenant_id:$tenant}) WHERE p.invoice_email_status IN ['queued','retry','sending']
         AND coalesce(p.invoice_email_attempts,0)<$max
         AND (p.invoice_email_next_at IS NULL OR p.invoice_email_next_at<=datetime())
         AND p.status IN ['pending','awaiting_proof'] AND (p.expires_at IS NULL OR p.expires_at>datetime())
         RETURN p.payment_id AS id LIMIT 20")
        .param("tenant",state.tenant_id.clone()).param("max",MAX_ATTEMPTS)).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Some(id) = row.get::<String>("id") {
            ids.push(id);
        }
    }
    for id in ids {
        let nonce = uuid::Uuid::new_v4().to_string();
        let Some(email) = claim(graph, &state.tenant_id, &id, &nonce).await? else {
            continue;
        };
        let payment =
            payment_repository::find_by_id_for_tenant(graph, &id, &state.tenant_id).await?;
        let sent = if let Some(payment) = payment.filter(|_| !email.trim().is_empty()) {
            let (subject, body) = invoice_content(&payment, &state.frontend_url);
            client.post(format!("{}/api/email/v1/send",state.notification_service_url.trim_end_matches('/')))
                .json(&serde_json::json!({"idempotencyKey":format!("invoice:{}:{}:email",state.tenant_id,id),"email":email,"subject":subject,"body":body}))
                .send().await.map(|r|r.status().is_success()).unwrap_or(false)
        } else {
            false
        };
        complete(graph, &state.tenant_id, &id, &nonce, sent).await?;
    }
    Ok(())
}

fn invoice_content(payment: &Payment, frontend: &str) -> (String, String) {
    let reference = payment
        .manual_reference
        .as_deref()
        .or(payment.invoice_ref.as_deref())
        .unwrap_or(&payment.payment_id);
    let label = match payment.payment_type.as_str() {
        "enrolment_fee" => "biaya enrolment",
        "application_fee" => "biaya pendaftaran",
        _ => "biaya sekolah",
    };
    let portal = format!("{}/parent/dashboard", frontend.trim_end_matches('/'));
    let bank = if payment.payment_method.as_deref() == Some("manual_transfer") {
        format!("\nTransfer ke {}\nNomor rekening: {}\nAtas nama: {}\nUnggah bukti pembayaran melalui portal. Pembayaran menunggu verifikasi finance.\n",payment.bank_name.as_deref().unwrap_or("-"),payment.bank_account_number.as_deref().unwrap_or("-"),payment.bank_account_name.as_deref().unwrap_or("-"))
    } else {
        String::new()
    };
    (format!("Tagihan {label} — {reference}"),format!("Tagihan {label} sudah tersedia.\nReferensi: {reference}\nTotal: {} {}\nJatuh tempo: {}\n{bank}\nLihat rincian dan lanjutkan pembayaran setelah masuk portal:\n{portal}\n\nEmail ini adalah tagihan, bukan bukti pembayaran lunas.",payment.currency,payment.amount,payment.expires_at.as_deref().unwrap_or("Lihat portal")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires disposable Neo4j on localhost:17687"]
    async fn durable_claim_is_tenant_scoped_and_sent_is_not_replayed() {
        let graph = Graph::new("bolt://127.0.0.1:17687", "neo4j", "local-test-only")
            .await
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        graph.run(query("CREATE (l:Lead {tenant_id:$id,email:'synthetic@example.invalid'})-[:MADE_PAYMENT]->(p:Payment {tenant_id:$id,payment_id:$id,status:'awaiting_proof',expires_at:datetime()+duration({days:1}),invoice_email_status:'queued'})").param("id",id.clone())).await.unwrap();
        // Exercise the accepted-offer creation path, not just a hand-built queue.
        let offer_id = format!("offer-{id}");
        let invoice_id = format!("invoice-{id}");
        graph.run(query("MATCH (l:Lead {tenant_id:$id}) SET l.lead_id=$id CREATE (o:Offer {offer_id:$offer,tenant_id:$id,status:'accepted',revision:1,pricing_snapshot_hash:'synthetic-hash'})-[:HAS_PAYMENT_SLOT]->(:OfferPaymentSlot {payment_id:$payment,payment_method:'manual_transfer',pricing_snapshot_hash:'synthetic-hash',tenant_id:$id})").param("id",id.clone()).param("offer",offer_id.clone()).param("payment",invoice_id.clone())).await.unwrap();
        let offer = payment_repository::AcceptedOfferSnapshot {
            offer_id,
            offer_revision: 1,
            lead_id: id.clone(),
            pricing_snapshot_hash: "synthetic-hash".into(),
            pricing_snapshot_json: "{}".into(),
        };
        let bank = payment_repository::ManualBankDetails {
            bank_account_id: "test".into(),
            bank_name: "Test".into(),
            account_name: "School".into(),
            account_number: "123".into(),
            instructions: "Transfer".into(),
        };
        assert!(payment_repository::upsert_offer_manual_pending(
            &graph,
            &invoice_id,
            &id,
            &offer,
            1000,
            "IDR",
            &(chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339(),
            "INV-TEST",
            &bank
        )
        .await
        .unwrap());
        assert!(claim(&graph, &id, &invoice_id, "offer-invoice")
            .await
            .unwrap()
            .is_some());
        complete(&graph, &id, &invoice_id, "offer-invoice", true)
            .await
            .unwrap();
        assert!(claim(&graph, "other", &id, "x").await.unwrap().is_none());
        assert!(claim(&graph, &id, &id, "first").await.unwrap().is_some());
        assert!(claim(&graph, &id, &id, "second").await.unwrap().is_none());
        complete(&graph, &id, &id, "wrong", true).await.unwrap();
        complete(&graph, &id, &id, "first", false).await.unwrap();
        graph.run(query("MATCH (p:Payment {payment_id:$id}) SET p.invoice_email_next_at=datetime()-duration({seconds:1})").param("id",id.clone())).await.unwrap();
        assert!(claim(&graph, &id, &id, "retry").await.unwrap().is_some());
        complete(&graph, &id, &id, "retry", true).await.unwrap();
        assert!(claim(&graph, &id, &id, "again").await.unwrap().is_none());
        graph
            .run(query("MATCH (n) WHERE n.tenant_id=$id DETACH DELETE n").param("id", id))
            .await
            .unwrap();
    }
    #[test]
    fn manual_invoice_has_actual_amount_and_bank_without_claiming_paid() {
        let payment: Payment=serde_json::from_value(serde_json::json!({"paymentId":"p","tenantId":"t","paymentType":"enrolment_fee","status":"awaiting_proof","amount":1200000,"currency":"IDR","paymentMethod":"manual_transfer","manualReference":"INV-1","bankName":"Test Bank","bankAccountNumber":"123","bankAccountName":"School"})).unwrap();
        let (subject, body) = invoice_content(&payment, "https://school.test/");
        assert!(subject.contains("INV-1"));
        assert!(body.contains("IDR 1200000"));
        assert!(body.contains("Nomor rekening: 123"));
        assert!(body.contains("bukan bukti pembayaran lunas"));
        assert!(body.contains("https://school.test/parent/dashboard"));
    }
}
