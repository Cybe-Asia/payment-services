//! Opt-in real HTTP + Neo4j acceptance. Creates only a UUID-scoped synthetic family/tenant.
use super::*;
use axum::{
    routing::{get, post},
    Router,
};
use neo4rs::{query, Graph};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires disposable ADMISSIONS_TEST_NEO4J_URI; no providers or notifications"]
async fn parent_manual_payment_http_saved_retry_and_revocation() {
    let graph = Arc::new(
        Graph::new(
            std::env::var("ADMISSIONS_TEST_NEO4J_URI").unwrap().as_str(),
            std::env::var("ADMISSIONS_TEST_NEO4J_USER")
                .unwrap()
                .as_str(),
            std::env::var("ADMISSIONS_TEST_NEO4J_PASSWORD")
                .unwrap()
                .as_str(),
        )
        .await
        .unwrap(),
    );
    let key = uuid::Uuid::new_v4().to_string();
    let tenant = format!("parent-payment-test-{key}");
    let user = format!("user-{key}");
    let stranger = format!("stranger-{key}");
    let lead = format!("LEAD-{key}");
    let foreign_lead = format!("LEAD-foreign-{key}");
    let student = format!("STU-{key}");
    let foreign_student = format!("STU-foreign-{key}");
    let school = format!("SCH-{key}").to_uppercase();
    graph.run(query("CREATE (u:User {id:$user,email:'owner@example.test',qa_key:$key})-[:HAS_APPLICATION]->(l:Lead {lead_id:$lead,tenant_id:$tenant,parent_name:'Synthetic parent',email:'owner@example.test',target_school_preference:$school,qa_key:$key})-[:HAS_STUDENT]->(:Student {studentId:$student,fullName:'Synthetic child',qa_key:$key}) CREATE (:User {id:$stranger,email:'stranger@example.test',qa_key:$key})-[:HAS_APPLICATION]->(:Lead {lead_id:$foreign_lead,tenant_id:$tenant,email:'stranger@example.test',qa_key:$key})-[:HAS_STUDENT]->(:Student {studentId:$foreign_student,qa_key:$key}) CREATE (s:School {school_id:$school,school_code:$school,tenant_id:$tenant,qa_key:$key})-[:HAS_FEE_STRUCTURE]->(:FeeStructure {fee_structure_id:$key,tenant_id:$tenant,school_id:$school,payment_type:'application_fee',amount:100,currency:'IDR',status:'active',effective_from:datetime(),qa_key:$key})")
        .param("key",key.clone()).param("user",user.clone()).param("stranger",stranger.clone()).param("lead",lead.clone()).param("foreign_lead",foreign_lead.clone()).param("student",student.clone()).param("foreign_student",foreign_student.clone()).param("school",school.clone()).param("tenant",tenant.clone())).await.unwrap();
    let secret = uuid::Uuid::new_v4().to_string();
    let token = |sub: &str| {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &json!({"sub":sub,"exp":chrono::Utc::now().timestamp()+600}),
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    };
    let owned_token = token(&user);
    let foreign_token = token(&stranger);
    let state = AppState {
        graph: Some(graph.clone()),
        xendit: crate::clients::xendit::XenditClient::new("http://127.0.0.1:1", "", "", ""),
        doku: crate::clients::doku::DokuClient::new("http://127.0.0.1:1", "", "", "", "", vec![]),
        doku_client_id: String::new(),
        doku_secret_key: String::new(),
        legacy_parent_payments_enabled: true,
        http_client: reqwest::Client::new(),
        tenant_id: tenant.clone(),
        xendit_webhook_token: String::new(),
        default_currency: "IDR".into(),
        default_due_hours: 24,
        jwt_secret: secret,
        notification_service_url: "http://127.0.0.1:1".into(),
        frontend_url: "http://127.0.0.1".into(),
        minio: None,
        payment_settings_seed:
            crate::repositories::payment_settings_repository::PaymentSettingsSeed {
                tenant_id: tenant.clone(),
                bank_name: "SYNTHETIC BANK".into(),
                bank_account_name: "SYNTHETIC SCHOOL".into(),
                bank_account_number: "0000000000".into(),
                instructions: "Synthetic fixture, do not transfer".into(),
            },
    };
    let app = Router::new()
        .route("/manual", post(create_manual_payment_handler))
        .route("/payment/:payment_id", get(get_payment_handler))
        .route(
            "/payment/:payment_id/invoice",
            get(download_invoice_document_handler),
        )
        .route(
            "/payment/:payment_id/receipt",
            get(download_receipt_document_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let payload = json!({"admissionId":lead,"paymentType":"application_fee"});
    let created = client
        .post(format!("{base}/manual"))
        .bearer_auth(&owned_token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    let status = created.status();
    let body = created.text().await.unwrap();
    assert_eq!(status, 200, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let payment = created["data"]["payment"].clone();
    let id = payment["paymentId"].as_str().unwrap();
    assert_eq!(payment["status"], "awaiting_proof");
    assert_eq!(payment["amount"], 100);
    let replay: serde_json::Value = client
        .post(format!("{base}/manual"))
        .bearer_auth(&owned_token)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["data"]["payment"]["paymentId"], id);
    let read: serde_json::Value = client
        .get(format!("{base}/payment/{id}"))
        .bearer_auth(&owned_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read["data"]["status"], "awaiting_proof");
    assert!(read["data"]["paidAt"].is_null());
    assert_eq!(
        client
            .get(format!("{base}/payment/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/payment/{id}"))
            .bearer_auth(&foreign_token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let invoice = client
        .get(format!("{base}/payment/{id}/invoice"))
        .bearer_auth(&owned_token)
        .send()
        .await
        .unwrap();
    assert_eq!(invoice.status(), 200);
    assert!(invoice.bytes().await.unwrap().starts_with(b"%PDF"));
    assert_eq!(
        client
            .get(format!("{base}/payment/{id}/receipt"))
            .bearer_auth(&owned_token)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .post(format!("{base}/manual"))
            .bearer_auth(&owned_token)
            .json(&json!({"admissionId":foreign_student,"paymentType":"application_fee"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // Even an owned application in another tenant must not create a payment here.
    graph.run(query("MATCH(u:User {id:$user}) CREATE (u)-[:HAS_APPLICATION]->(:Lead {lead_id:$other,tenant_id:'other-tenant',email:'owner@example.test',qa_key:$key})")
        .param("user", user.clone()).param("other", format!("LEAD-other-{key}")).param("key", key.clone())).await.unwrap();
    assert_eq!(
        client
            .post(format!("{base}/manual"))
            .bearer_auth(&owned_token)
            .json(
                &json!({"admissionId":format!("LEAD-other-{key}"),"paymentType":"application_fee"})
            )
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // A different tenant's active payment linked to this Lead is never reused.
    graph.run(query("MATCH(l:Lead {lead_id:$lead}) CREATE (l)-[:MADE_PAYMENT]->(:Payment {payment_id:$other,tenant_id:'other-tenant',payment_type:'application_fee',payment_method:'manual_transfer',status:'awaiting_proof',amount:999,currency:'IDR',created_at:datetime()+duration('P1D'),qa_key:$key})")
        .param("lead",lead.clone()).param("other",format!("PAY-other-{key}")).param("key",key.clone())).await.unwrap();
    let retry: serde_json::Value = client
        .post(format!("{base}/manual"))
        .bearer_auth(&owned_token)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["data"]["payment"]["paymentId"], id);
    // Revocation is observed on the next actual HTTP read; persisted payment stays intact.
    graph
        .run(
            query("MATCH (:User {id:$user})-[r:HAS_APPLICATION]->(:Lead {lead_id:$lead}) DELETE r")
                .param("user", user.clone())
                .param("lead", lead.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/payment/{id}"))
            .bearer_auth(&owned_token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    for document in ["invoice", "receipt"] {
        assert_eq!(
            client
                .get(format!("{base}/payment/{id}/{document}"))
                .bearer_auth(&owned_token)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    let saved = crate::repositories::payment_repository::find_by_id_for_tenant(&graph, id, &tenant)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "awaiting_proof");
    server.abort();
    graph
        .run(
            query("MATCH (n) WHERE n.qa_key=$key OR n.tenant_id=$tenant DETACH DELETE n")
                .param("key", key)
                .param("tenant", tenant),
        )
        .await
        .unwrap();
}
