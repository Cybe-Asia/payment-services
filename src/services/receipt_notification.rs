//! Durable IIEC paid application-fee receipts. Enqueued atomically at verification.
use crate::models::payment::Payment;
use crate::repositories::receipt_notification_repository::{self, claim, complete};
use crate::{repositories::payment_repository, AppState};
#[cfg(test)]
use neo4rs::query;
use neo4rs::Graph;

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

async fn deliver_batch(
    graph: &Graph,
    state: &AppState,
    client: &reqwest::Client,
) -> Result<(), neo4rs::Error> {
    let ids = receipt_notification_repository::pending_ids(graph, &state.tenant_id).await?;
    for id in ids {
        let nonce = uuid::Uuid::new_v4().to_string();
        let Some(email) = claim(graph, &state.tenant_id, &id, &nonce).await? else {
            continue;
        };
        let payment =
            payment_repository::find_by_id_for_tenant(graph, &id, &state.tenant_id).await?;
        let sent = if let Some(payment) = payment.filter(|_| !email.trim().is_empty()) {
            let (subject, body, html) = receipt_content(&payment, &state.frontend_url);
            client.post(format!("{}/api/email/v1/send",state.notification_service_url.trim_end_matches('/')))
                .json(&serde_json::json!({"idempotencyKey":format!("receipt:{}:{}:email",state.tenant_id,id),"email":email,"subject":subject,"body":body,"html":html}))
                .send().await.map(|r|r.status().is_success()).unwrap_or(false)
        } else {
            false
        };
        complete(graph, &state.tenant_id, &id, &nonce, sent).await?;
    }
    Ok(())
}

fn money(currency: &str, amount: i64) -> String {
    let digits=amount.to_string();
    let mut grouped=String::new();
    for (i,c) in digits.chars().rev().enumerate() { if i>0 && i%3==0 { grouped.push('.'); } grouped.push(c); }
    format!("{} {}",if currency=="IDR" {"Rp"} else {currency},grouped.chars().rev().collect::<String>())
}

fn receipt_content(payment: &Payment, frontend: &str) -> (String, String, String) {
    let reference = payment
        .invoice_ref
        .as_deref()
        .or(payment.manual_reference.as_deref())
        .unwrap_or(&payment.payment_id);
    let portal = format!("{}/parent/dashboard", frontend.trim_end_matches('/'));
    let paid_at = payment
        .paid_at
        .as_deref()
        .or(payment.reviewed_at.as_deref())
        .unwrap_or("Terverifikasi");
    let paid_at=chrono::DateTime::parse_from_rfc3339(paid_at).map(|date| date.with_timezone(&chrono::FixedOffset::east_opt(7*3600).unwrap()).format("%d %b %Y, %H:%M WIB").to_string()).unwrap_or_else(|_| paid_at.to_string());
    let verified = payment.amount_verified.unwrap_or(payment.amount);
    let body = format!("IIEC School\nPEMBAYARAN TERVERIFIKASI — INVOICE LUNAS\n\nPembayaran application fee (biaya pendaftaran) telah terverifikasi.\n\nNomor invoice/referensi: {reference}\nID pembayaran: {}\nTotal tagihan: {}\nJumlah terverifikasi: {}\nTanggal verifikasi/pembayaran: {paid_at}\nStatus: LUNAS\n\nKode tes untuk setiap anak akan dikirim dalam email terpisah ketika akses tes siap. Anda juga dapat membuka atau memilih tes melalui portal parent.\n\nBuka portal: {portal}\n\nSimpan email ini sebagai catatan pembayaran application fee IIEC.", payment.payment_id,money(&payment.currency,payment.amount),money(&payment.currency,verified));
    let html = crate::utils::branded_email::branded_html(&body, frontend);
    (
        format!("IIEC — Pembayaran application fee terverifikasi | {reference}"),
        body,
        html,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI"]
    async fn receipt_transition_retry_tenant_and_replay_contract() {
        let graph = Graph::new(
            std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
            "neo4j",
            "test",
        )
        .await
        .unwrap();
        let id = format!("receipt-{}", uuid::Uuid::new_v4());
        graph.run(query("CREATE (l:Lead {tenant_id:$id,lead_id:$id,email:'parent@example.invalid'})-[:MADE_PAYMENT]->(:Payment {tenant_id:$id,payment_id:$id,status:'pending',payment_type:'application_fee'})").param("id",id.clone())).await.unwrap();
        assert!(claim(&graph, &id, &id, "unpaid").await.unwrap().is_none());
        payment_repository::mark_paid(&graph, &id, None, None)
            .await
            .unwrap();
        assert!(claim(&graph, "wrong-tenant", &id, "wrong")
            .await
            .unwrap()
            .is_none());
        let (first, second) = tokio::join!(
            claim(&graph, &id, &id, "first"),
            claim(&graph, &id, &id, "second")
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.is_some(), second.is_some());
        let winner = if first.is_some() { "first" } else { "second" };
        complete(&graph, &id, &id, winner, false).await.unwrap();
        assert!(claim(&graph, &id, &id, "early").await.unwrap().is_none());
        graph.run(query("MATCH (p:Payment {payment_id:$id}) SET p.receipt_email_next_at=datetime()-duration({seconds:1})").param("id",id.clone())).await.unwrap();
        assert!(claim(&graph, &id, &id, "retry").await.unwrap().is_some());
        complete(&graph, &id, &id, winner, true).await.unwrap();
        complete(&graph, &id, &id, "retry", true).await.unwrap();
        payment_repository::mark_paid(&graph, &id, None, None)
            .await
            .unwrap();
        assert!(claim(&graph, &id, &id, "duplicate")
            .await
            .unwrap()
            .is_none());
        // Replay on a historical paid payment cannot create a new receipt or test-code trigger.
        graph.run(query("MATCH (p:Payment {payment_id:$id}) SET p.receipt_email_status=null,p.paid_at=datetime('2026-01-01T00:00:00Z')").param("id",id.clone())).await.unwrap();
        payment_repository::mark_paid(&graph, &id, None, None)
            .await
            .unwrap();
        let mut rows=graph.execute(query("MATCH (p:Payment {payment_id:$id}) RETURN p.receipt_email_status IS NULL AS absent,toString(p.paid_at) AS paid").param("id",id.clone())).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<bool>("absent"), Some(true));
        assert!(row.get::<String>("paid").unwrap().starts_with("2026-01-01"));
        graph.run(query("MATCH (p:Payment {payment_id:$id}) SET p.status='pending_verification',p.payment_method='manual_transfer',p.receipt_email_next_at=null,p.receipt_email_attempts=0 CREATE (p)-[:HAS_PROOF]->(:PaymentProof {tenant_id:$id,status:'uploaded',amount_submitted:100})").param("id",id.clone())).await.unwrap();
        assert!(payment_repository::review_manual_payment(
            &graph,
            payment_repository::ManualPaymentReviewUpdate {
                payment_id: &id,
                tenant_id: &id,
                payment_status: "paid",
                proof_status: "approved",
                amount_verified: 100,
                short_amount: 0,
                overpaid_amount: 0,
                note: None,
                rejection_reason: None,
                reviewed_by: "synthetic-finance",
                receipt_ref: None
            }
        )
        .await
        .unwrap());
        assert!(claim(&graph, &id, &id, "manual").await.unwrap().is_some());
        graph.run(query("MATCH (p:Payment {payment_id:$id}) SET p.status='pending',p.provider='doku',p.receipt_email_status=null,p.receipt_email_next_at=null,p.receipt_email_attempts=0").param("id",id.clone())).await.unwrap();
        assert!(
            payment_repository::apply_doku_webhook(&graph, &id, &id, "paid", None)
                .await
                .unwrap()
        );
        assert!(claim(&graph, &id, &id, "doku").await.unwrap().is_some());
        complete(&graph, &id, &id, "doku", true).await.unwrap();
        assert!(
            !payment_repository::apply_doku_webhook(&graph, &id, &id, "paid", None)
                .await
                .unwrap()
        );
        assert!(claim(&graph, &id, &id, "doku-replay")
            .await
            .unwrap()
            .is_none());
        graph
            .run(
                query("MATCH (r:DokuWebhookReceipt {request_id:$id}) DELETE r")
                    .param("id", id.clone()),
            )
            .await
            .unwrap();
        graph
            .run(query("MATCH (n {tenant_id:$id}) DETACH DELETE n").param("id", id))
            .await
            .unwrap();
    }

    #[test]
    fn receipt_has_invoice_reference_verified_amount_and_separate_test_email() {
        let payment: Payment = serde_json::from_value(serde_json::json!({"paymentId":"PAY-test", "tenantId":"test", "paymentType":"application_fee", "status":"paid", "amount":2200000, "amountVerified":2200000, "currency":"IDR", "invoiceRef":"INV-<test>", "paidAt":"2026-09-12T10:00:00Z"})).unwrap();
        let (subject, body, html) = receipt_content(&payment, "https://school.test");
        assert!(subject.contains("terverifikasi"));
        assert!(html.contains("/brand/iiec-logo.png"));
        if let Ok(path) = std::env::var("EMAIL_PREVIEW_PATH") {
            std::fs::write(path, &html).unwrap();
        }
        assert!(
            body.contains("Rp 2.200.000")
                && body.contains("INVOICE LUNAS")
                && body.contains("email terpisah")
        );
        assert!(html.contains("INV-&lt;test&gt;") && !html.contains("INV-<test>"));
    }
}
