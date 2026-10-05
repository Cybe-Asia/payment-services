//! Actual owning OTP-issued Parent session and Auth-issued staff fixture.
//! Uses only an explicitly owned disposable graph and existing encrypted store.
use super::*;
use neo4rs::{query, Graph};
use serde_json::{json, Value};
use std::sync::Arc;
fn private_json(path: &str) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn string<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap()
}
#[tokio::test]
#[ignore = "requires SCHOOL_INVOICE_HTTP_FIXTURE with actual OTP/Auth credentials and owned disposable graph"]
async fn actual_owner_http_proof_receipt_and_scope_denials() {
    let f = private_json(&std::env::var("SCHOOL_INVOICE_HTTP_FIXTURE").unwrap());
    let env = private_json(string(&f, "privateEnv"));
    let staff = private_json(string(&f, "financeFixture"));
    let owner = private_json(string(&f, "ownerFixture"));
    let uri = string(&env, "NEO4J_URI");
    assert!(uri.starts_with("bolt://127.0.0.1:"));
    assert_ne!(uri, string(&f, "primaryGraphUri"));
    std::env::set_var("STAFF_DOWNSTREAM_JWT_SECRET", string(&staff, "staff_key"));
    std::env::set_var("STAFF_DOWNSTREAM_JWT_ISSUER", string(&staff, "issuer"));
    let g = Arc::new(
        Graph::new(
            uri,
            string(&env, "NEO4J_USER"),
            string(&env, "NEO4J_PASSWORD"),
        )
        .await
        .unwrap(),
    );
    repository::init(&g).await.unwrap();
    let marker = uuid::Uuid::new_v4().to_string();
    let payer = string(&f, "user");
    let lead = string(&f, "lead");
    let child = format!("http-child-{marker}");
    let enrolled = format!("http-enrolled-{marker}");
    let school = format!("http-school-{marker}");
    let tenant = format!("http-tenant-{marker}");
    let mut check = g
        .execute(query("MATCH(u:User {id:$id}) RETURN count(u) AS count").param("id", payer))
        .await
        .unwrap();
    assert_eq!(
        check.next().await.unwrap().unwrap().get::<i64>("count"),
        Some(0),
        "Use an owned actual OTP principal not already retained in this disposable graph"
    );
    let bank=json!([{"id":"synthetic-existing-bank","bankName":"Synthetic Bank","accountName":"Synthetic School (NO TRANSFER)","accountNumber":"0000000000","instructions":"No money is transferred. Contract acceptance only.","enabled":true}]).to_string();
    g.run(query("CREATE(u:User {id:$payer,role:'parent',email:$email,fullName:'Synthetic Parent',billing_test:$marker})-[:HAS_APPLICATION]->(l:Lead {lead_id:$lead,email:$email,status:'verified',billing_test:$marker})-[:HAS_STUDENT]->(s:Student {studentId:$child,fullName:'Synthetic Enrolled Child',applicantStatus:'handed_to_sis',billing_test:$marker}) CREATE(:ParentLoginChallenge {id:$challenge,purpose:'parent_login',channel:'email',userId:$payer,leadId:$lead,email:$email,isUsed:true,delivered:true,billing_test:$marker}) CREATE(s)-[:ENROLLED_AS]->(:EnrolledStudent {student_id:$enrolled,applicant_student_id:$child,status:'active',school_id:$school,tenant_id:$tenant,billing_test:$marker})-[:ENROLLED_IN]->(:Section {section_id:$section,status:'active',school_id:$school,tenant_id:$tenant,billing_test:$marker}) CREATE(:School {school_id:$school,tenant_id:$tenant,school_code:'IISS',name:'Synthetic School',billing_test:$marker}) CREATE(:PaymentSettings {tenant_id:$tenant,manual_transfer_enabled:true,manual_bank_accounts_json:$bank,billing_test:$marker}) CREATE(:User {id:$owner,billing_test:$marker})-[:STAFF_MEMBER]->(:StaffMember {id:$ownerstaff,membershipStatus:'ACTIVE',roles:['owner'],schoolIds:[$school],tenantIds:[$tenant],teamIds:[],billing_test:$marker}) CREATE(:User {id:$finance,billing_test:$marker})-[:STAFF_MEMBER]->(:StaffMember {id:$financestaff,membershipStatus:'ACTIVE',roles:['finance_admin','finance_approver'],schoolIds:[$school],tenantIds:[$tenant],teamIds:[],billing_test:$marker})")
 .param("payer",payer).param("lead",lead).param("email",string(&f,"email")).param("challenge",string(&f,"challenge")).param("child",child.clone()).param("enrolled",enrolled.clone()).param("school",school.clone()).param("tenant",tenant.clone()).param("section",format!("http-section-{marker}")).param("owner",string(&owner,"user_id")).param("ownerstaff",string(&owner,"staff_id")).param("finance",string(&staff,"user_id")).param("financestaff",string(&staff,"staff_id")).param("bank",bank).param("marker",marker.clone())).await.unwrap();
    let cipher = crate::services::document_encryption::DocumentCipher::from_hex_keyring(
        string(&env, "DOCUMENT_ENCRYPTION_PRIMARY_KEY_ID"),
        string(&env, "DOCUMENT_ENCRYPTION_KEYRING"),
        false,
    )
    .unwrap();
    let minio = crate::clients::minio::MinioClient::new(
        string(&env, "MINIO_ENDPOINT"),
        string(&env, "MINIO_REGION"),
        string(&env, "MINIO_ACCESS_KEY"),
        string(&env, "MINIO_SECRET_KEY"),
        string(&env, "MINIO_BUCKET"),
        cipher,
    )
    .await;
    let state = AppState {
        graph: Some(g.clone()),
        xendit: crate::clients::xendit::XenditClient::new("http://127.0.0.1:1", "", "", ""),
        doku: crate::clients::doku::DokuClient::new("http://127.0.0.1:1", "", "", "", "", vec![]),
        doku_client_id: String::new(),
        doku_secret_key: String::new(),
        legacy_parent_payments_enabled: false,
        http_client: reqwest::Client::new(),
        tenant_id: tenant.clone(),
        xendit_webhook_token: String::new(),
        default_currency: "IDR".into(),
        default_due_hours: 24,
        jwt_secret: string(&f, "parentKey").into(),
        notification_service_url: "http://127.0.0.1:1".into(),
        frontend_url: "http://127.0.0.1".into(),
        minio: Some(minio.clone()),
        payment_settings_seed:
            crate::repositories::payment_settings_repository::PaymentSettingsSeed {
                tenant_id: tenant.clone(),
                bank_name: String::new(),
                bank_account_name: String::new(),
                bank_account_number: String::new(),
                instructions: String::new(),
            },
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v1/payments", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router().with_state(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let parent = string(&f, "parentToken");
    let finance = string(&staff, "token");
    let ownertoken = string(&owner, "token");
    assert_eq!(
        client
            .get(format!("{base}/school-invoices"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{base}/admin/school-invoices"))
            .bearer_auth(parent)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let config = json!({"tenantId":tenant,"schoolId":school,"bankAccountId":"synthetic-existing-bank","version":0});
    assert_eq!(
        client
            .post(format!("{base}/admin/school-invoices/payee"))
            .bearer_auth(finance)
            .json(&config)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{base}/admin/school-invoices/payee"))
            .bearer_auth(ownertoken)
            .json(&config)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let input = json!({"idempotencyKey":marker,"studentId":child,"payerUserId":payer,"period":"October 2026","description":"Explicit synthetic tuition","amount":100,"currency":"IDR","dueDate":"2026-10-10","bankAccountId":"synthetic-existing-bank"});
    let res = client
        .post(format!("{base}/admin/school-invoices"))
        .bearer_auth(finance)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let created: Value = res.json().await.unwrap();
    let id = string(&created["data"], "id");
    let replay: Value = client
        .post(format!("{base}/admin/school-invoices"))
        .bearer_auth(finance)
        .json(&input)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["data"]["id"], id);
    let mut conflict = input.clone();
    conflict["amount"] = json!(101);
    assert_eq!(
        client
            .post(format!("{base}/admin/school-invoices"))
            .bearer_auth(finance)
            .json(&conflict)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let detail = format!("{base}/school-invoices/{id}");
    assert_eq!(
        client
            .get(&detail)
            .bearer_auth(parent)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{detail}/receipt.pdf"))
            .bearer_auth(parent)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .get(format!("{base}/school-invoices/SINV-foreign"))
            .bearer_auth(parent)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let invoice = client
        .get(format!("{detail}/invoice.pdf"))
        .bearer_auth(parent)
        .send()
        .await
        .unwrap();
    assert_eq!(invoice.status(), 200);
    assert!(invoice.bytes().await.unwrap().starts_with(b"%PDF-"));
    // A scope removal applies to the next HTTP boundary with the same actual token.
    g.run(
        query("MATCH(m:StaffMember {id:$id}) SET m.tenantIds=['foreign-tenant']")
            .param("id", string(&staff, "staff_id")),
    )
    .await
    .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/admin/school-invoices/{id}"))
            .bearer_auth(finance)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    g.run(
        query("MATCH(m:StaffMember {id:$id}) SET m.tenantIds=[$tenant]")
            .param("id", string(&staff, "staff_id"))
            .param("tenant", tenant.clone()),
    )
    .await
    .unwrap();
    let boundary = format!("school-proof-{marker}");
    let file = std::fs::read(string(&f, "proofFile")).unwrap();
    let mut body = vec![];
    for (key, val) in [
        ("amountSubmitted", "100"),
        ("paidAt", "2026-10-04"),
        ("payerName", "SYNTHETIC QA"),
        ("payerBank", "SYNTHETIC BANK"),
        ("referenceNumber", "NO REAL MONEY TRANSFER"),
    ] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{val}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"no-real-money.pdf\"\r\nContent-Type: application/pdf\r\n\r\n").as_bytes());
    body.extend_from_slice(&file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let upload = client
        .post(format!("{detail}/proofs"))
        .bearer_auth(parent)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 200);
    let submitted: Value = upload.json().await.unwrap();
    assert_eq!(submitted["data"]["status"], "pending_verification");
    assert_eq!(
        client
            .post(format!("{detail}/proofs"))
            .bearer_auth(parent)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}")
            )
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let admin = format!("{base}/admin/school-invoices/{id}");
    let record: Value = client
        .get(&admin)
        .bearer_auth(finance)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let proofid = string(&record["data"]["proofs"][0], "id");
    let proof = client
        .get(format!("{detail}/proofs/{proofid}/download"))
        .bearer_auth(parent)
        .send()
        .await
        .unwrap();
    assert_eq!(proof.status(), 200);
    assert_eq!(proof.bytes().await.unwrap().as_ref(), file.as_slice());
    // Audit confirms every matching stored evidence envelope authenticates.
    let (encrypted, _) = minio.audit_documents(false).await.unwrap();
    assert!(encrypted >= 1);
    let decision = json!({"version":1,"proofId":proofid,"decision":"approve","note":"Synthetic acceptance; no money transferred"});
    let approved = client
        .post(format!("{admin}/review"))
        .bearer_auth(finance)
        .json(&decision)
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), 200);
    assert_eq!(
        approved.json::<Value>().await.unwrap()["data"]["status"],
        "paid"
    );
    assert_eq!(
        client
            .post(format!("{admin}/review"))
            .bearer_auth(finance)
            .json(&decision)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let receipt = client
        .get(format!("{detail}/receipt.pdf"))
        .bearer_auth(parent)
        .send()
        .await
        .unwrap();
    assert_eq!(receipt.status(), 200);
    assert!(receipt.bytes().await.unwrap().starts_with(b"%PDF-"));
    g.run(
        query("MATCH(e:EnrolledStudent {student_id:$id}) SET e.status='graduated'")
            .param("id", enrolled),
    )
    .await
    .unwrap();
    for suffix in ["", "/invoice.pdf", "/receipt.pdf"] {
        assert_eq!(
            client
                .get(format!("{detail}{suffix}"))
                .bearer_auth(parent)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let mut facts=g.execute(query("MATCH(s:Student {studentId:$child}),(l:Lead {lead_id:$lead}) OPTIONAL MATCH(settlement:SchoolInvoiceSettlement {invoice_id:$invoice}) RETURN s.applicantStatus AS student_status,l.status AS lead_status,count(settlement) AS settlements").param("child",child).param("lead",lead).param("invoice",id)).await.unwrap();
    let fact = facts.next().await.unwrap().unwrap();
    assert_eq!(
        fact.get::<String>("student_status").unwrap(),
        "handed_to_sis"
    );
    assert_eq!(fact.get::<String>("lead_status").unwrap(), "verified");
    assert_eq!(fact.get::<i64>("settlements"), Some(1));
    let report = json!({"auth":"actual owning OTP Parent session + canonical Auth 900s exporter staff contract","scope":"owned disposable graph only; encrypted dedicated fixture store; ephemeral HTTP server","issued":200,"idempotentReplay":200,"conflictingReplay":409,"foreignParentInvoice":403,"financeForeignTenant":403,"proofUpload":200,"duplicateProof":409,"encryptedEvidenceVerified":true,"unpaidReceipt":409,"financeApproval":200,"duplicateReview":409,"paidReceipt":200,"postEnrollmentAccess":403,"postEnrollmentPolicy":"unconfigured fail closed","separateSettlements":1,"admissionsStateUnchanged":true,"providersCalled":false,"primaryGraphWrites":false});
    std::fs::write(
        string(&f, "evidenceFile"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    let object = format!("school-test/payments/{id}/{proofid}");
    minio.delete_object(&object).await.unwrap();
    g.run(query("MATCH(n) WHERE n.billing_test=$marker OR (n:SchoolInvoice AND n.tenant_id=$tenant) OR (n:SchoolBillingPayee AND n.tenant_id=$tenant) OR ((n:SchoolInvoiceAudit OR n:SchoolInvoiceProof OR n:SchoolInvoiceSettlement) AND (n.invoice_id=$invoice OR n.tenant_id=$tenant)) DETACH DELETE n").param("marker",marker).param("tenant",tenant).param("invoice",id)).await.unwrap();
    server.abort();
}
