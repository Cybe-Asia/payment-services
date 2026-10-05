use super::{
    auth::{Parent, Staff},
    model::*,
    payee, repository as repo,
};
use neo4rs::{query, Graph};
use serde_json::json;

#[tokio::test]
#[ignore = "requires explicitly owned disposable SCHOOL_INVOICE_TEST_NEO4J_URI"]
async fn current_family_scope_issuance_cas_and_separate_settlement() {
    let uri = std::env::var("SCHOOL_INVOICE_TEST_NEO4J_URI").expect("explicit test graph");
    assert!(uri.starts_with("bolt://127.0.0.1:"));
    let g = Graph::new(
        &uri,
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_USER").unwrap(),
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_PASSWORD").unwrap(),
    )
    .await
    .unwrap();
    repo::init(&g).await.unwrap();
    let mark = uuid::Uuid::new_v4().to_string();
    let tenant = format!("billing-tenant-{mark}");
    let school = format!("billing-school-{mark}");
    let student = format!("billing-child-{mark}");
    let enrolled = format!("billing-enrolled-{mark}");
    let payer = format!("billing-parent-{mark}");
    let owner = Staff {
        subject: format!("billing-owner-{mark}"),
        id: format!("billing-owner-staff-{mark}"),
        expires: chrono::Utc::now().timestamp() + 900,
    };
    let finance = Staff {
        subject: format!("billing-finance-{mark}"),
        id: format!("billing-finance-staff-{mark}"),
        expires: chrono::Utc::now().timestamp() + 900,
    };
    let parent = Parent {
        subject: payer.clone(),
        expires: chrono::Utc::now().timestamp() + 900,
    };
    let bank=json!([{"id":"bank-fixture","bankName":"Synthetic Bank","accountName":"Synthetic School — no money transfer","accountNumber":"0000000000","instructions":"Contract fixture, do not transfer money","enabled":true}]).to_string();
    g.run(query("CREATE(u:User {id:$payer,role:'parent',email:$email,fullName:'Synthetic Parent',billing_test:$mark})-[:HAS_APPLICATION]->(l:Lead {lead_id:$lead,email:$email,status:'handed_to_sis',otp_verified:true,billing_test:$mark})-[:HAS_STUDENT]->(s:Student {studentId:$student,fullName:'Synthetic Child',applicantStatus:'handed_to_sis',billing_test:$mark}) CREATE(s)-[:ENROLLED_AS]->(e:EnrolledStudent {student_id:$enrolled,applicant_student_id:$student,status:'active',school_id:$school,tenant_id:$tenant,billing_test:$mark})-[:ENROLLED_IN]->(:Section {section_id:$section,status:'active',school_id:$school,tenant_id:$tenant,billing_test:$mark}) CREATE(:School {school_id:$school,school_code:'IISS',tenant_id:$tenant,name:'Synthetic School',billing_test:$mark}) CREATE(:PaymentSettings {tenant_id:$tenant,manual_transfer_enabled:true,manual_bank_accounts_json:$bank,billing_test:$mark}) CREATE(:User {id:$owner,billing_test:$mark})-[:STAFF_MEMBER]->(:StaffMember {id:$ownerstaff,membershipStatus:'ACTIVE',roles:['owner'],tenantIds:[$tenant],schoolIds:[$school],teamIds:[],billing_test:$mark}) CREATE(:User {id:$finance,billing_test:$mark})-[:STAFF_MEMBER]->(:StaffMember {id:$financestaff,membershipStatus:'ACTIVE',roles:['finance_admin'],tenantIds:[$tenant],schoolIds:[$school],teamIds:[],billing_test:$mark})")
 .param("payer",payer.clone()).param("email",format!("billing-{mark}@example.test")).param("lead",format!("LEAD-{mark}")).param("student",student.clone()).param("enrolled",enrolled.clone()).param("section",format!("billing-section-{mark}")).param("tenant",tenant.clone()).param("school",school.clone()).param("owner",owner.subject.clone()).param("ownerstaff",owner.id.clone()).param("finance",finance.subject.clone()).param("financestaff",finance.id.clone()).param("bank",bank).param("mark",mark.clone())).await.unwrap();
    let mut input = Issue {
        idempotency_key: mark.clone(),
        student_id: student.clone(),
        payer_user_id: payer.clone(),
        period: "October 2026".into(),
        description: "Explicit school tuition".into(),
        amount: 100,
        currency: "IDR".into(),
        due_date: "2026-10-10".into(),
        bank_account_id: "bank-fixture".into(),
    };
    assert!(matches!(
        repo::issue(&g, &finance, &input).await,
        Err(repo::Error::Denied)
    ));
    let config = payee::Configure {
        tenant_id: tenant.clone(),
        school_id: school.clone(),
        bank_account_id: "bank-fixture".into(),
        version: 0,
    };
    assert!(matches!(
        payee::configure(&g, &finance, &config).await,
        Err(repo::Error::Denied)
    ));
    assert_eq!(
        payee::configure(&g, &owner, &config).await.unwrap().version,
        1
    );
    assert!(matches!(
        payee::configure(&g, &owner, &config).await,
        Err(repo::Error::Conflict)
    ));
    // An extra incoming identity without an ID must not evade ambiguity denial.
    g.run(query("MATCH(m:StaffMember {id:$id}) CREATE(:User {billing_test:$mark,billing_idless:true})-[:STAFF_MEMBER]->(m)").param("id",finance.id.clone()).param("mark",mark.clone())).await.unwrap();
    assert!(matches!(
        repo::issue(&g, &finance, &input).await,
        Err(repo::Error::Denied)
    ));
    assert!(matches!(
        repo::list_finance(&g, &finance).await,
        Err(repo::Error::Denied)
    ));
    g.run(
        query("MATCH(u:User {billing_test:$mark,billing_idless:true}) DETACH DELETE u")
            .param("mark", mark.clone()),
    )
    .await
    .unwrap();
    let directory = payee::list(&g, &owner).await.unwrap();
    assert_eq!(directory.len(), 1);
    assert_eq!(directory[0].version, 1);
    assert_eq!(directory[0].bank.as_ref().unwrap().id, "bank-fixture");
    let inv = repo::issue(&g, &finance, &input).await.unwrap();
    assert_eq!(inv.snapshot.enrolled_student_id, enrolled);
    assert_eq!(inv.status, "pending");
    assert_eq!(inv.snapshot.payee_version, 1);
    // A directory update held by Owner must not let issuance use a stale bank.
    let mut bank_input = input.clone();
    bank_input.idempotency_key = format!("bank-race-{mark}");
    let held = g.start_txn().await.unwrap();
    held.run(
        query("MATCH(ps:PaymentSettings {tenant_id:$tenant}) SET ps.manual_transfer_enabled=false")
            .param("tenant", tenant.clone()),
    )
    .await
    .unwrap();
    let queued = repo::issue(&g, &finance, &bank_input);
    tokio::pin!(queued);
    tokio::select! {_=&mut queued=>panic!("issuance did not wait for the current bank-directory write"),_ = tokio::time::sleep(std::time::Duration::from_millis(150))=>{}}
    held.commit().await.unwrap();
    assert!(queued.await.is_err());
    let mut drafts = g
        .execute(
            query("MATCH(i:SchoolInvoice {issue_key:$key}) RETURN count(i) AS count").param(
                "key",
                format!("{tenant}|{school}|{}", bank_input.idempotency_key),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        drafts.next().await.unwrap().unwrap().get::<i64>("count"),
        Some(0)
    );
    g.run(
        query("MATCH(ps:PaymentSettings {tenant_id:$tenant}) SET ps.manual_transfer_enabled=true")
            .param("tenant", tenant.clone()),
    )
    .await
    .unwrap();
    // A verified credential can expire while a Payment-owned lock is queued.
    let mut expiring = finance.clone();
    expiring.expires = chrono::Utc::now().timestamp() + 2;
    let mut expiry_input = input.clone();
    expiry_input.idempotency_key = format!("expiry-{mark}");
    let held = g.start_txn().await.unwrap();
    held.run(query("MATCH(ps:PaymentSettings {tenant_id:$tenant}) SET ps.school_billing_lock=coalesce(ps.school_billing_lock,0)+1").param("tenant",tenant.clone())).await.unwrap();
    let queued = repo::issue(&g, &expiring, &expiry_input);
    tokio::pin!(queued);
    tokio::select! {_=&mut queued=>panic!("issuance did not wait"),_=tokio::time::sleep(std::time::Duration::from_secs(2))=>{}}
    held.commit().await.unwrap();
    assert!(matches!(queued.await, Err(repo::Error::Expired)));
    let mut drafts = g
        .execute(
            query("MATCH(i:SchoolInvoice {issue_key:$key}) RETURN count(i) AS count").param(
                "key",
                format!("{tenant}|{school}|{}", expiry_input.idempotency_key),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        drafts.next().await.unwrap().unwrap().get::<i64>("count"),
        Some(0)
    );
    let (a, b) = tokio::join!(
        repo::issue(&g, &finance, &input),
        repo::issue(&g, &finance, &input)
    );
    assert_eq!(a.unwrap().snapshot.id, inv.snapshot.id);
    assert_eq!(b.unwrap().snapshot.id, inv.snapshot.id);
    input.amount = 101;
    assert!(matches!(
        repo::issue(&g, &finance, &input).await,
        Err(repo::Error::Conflict)
    ));
    input.amount = 100;
    assert_eq!(
        repo::parent(&g, &parent, &inv.snapshot.id)
            .await
            .unwrap()
            .version,
        0
    );
    assert!(matches!(
        repo::parent(
            &g,
            &Parent {
                subject: "foreign-parent".into(),
                expires: parent.expires
            },
            &inv.snapshot.id
        )
        .await,
        Err(repo::Error::Denied)
    ));
    assert!(super::documents::render(&inv, true).is_err());
    assert!(super::documents::render(&inv, false)
        .unwrap()
        .starts_with(b"%PDF-"));
    assert_eq!(repo::list_parent(&g, &parent).await.unwrap().len(), 1);
    g.run(
        query("CREATE(:User {id:$foreign,role:'parent',email:$email,billing_test:$mark})")
            .param("foreign", format!("foreign-parent-{mark}"))
            .param("email", format!("foreign-{mark}@example.test"))
            .param("mark", mark.clone()),
    )
    .await
    .unwrap();
    // A payer selector alone is not a legitimate issuance. Imported/unaudited
    // records never acquire historical access, even for a real Parent account.
    let unaudited = format!("SINV-unaudited-{mark}");
    g.run(query("MATCH(i:SchoolInvoice {id:$id}) CREATE(fake) SET fake=properties(i),fake.id=$fake,fake.issue_key=$key SET fake:SchoolInvoice")
        .param("id",inv.snapshot.id.clone()).param("fake",unaudited.clone()).param("key",format!("{tenant}|{school}|unaudited-{mark}"))).await.unwrap();
    assert!(matches!(
        repo::parent(&g, &parent, &unaudited).await,
        Err(repo::Error::Denied)
    ));
    assert_eq!(repo::list_parent(&g, &parent).await.unwrap().len(), 1);
    g.run(query("MATCH(i:SchoolInvoice {id:$fake}) DETACH DELETE i").param("fake", unaudited))
        .await
        .unwrap();
    // Snapshot selectors and the original command must bind the same student.
    g.run(
        query("MATCH(i:SchoolInvoice {id:$id}) SET i.student_id='unrelated-child'")
            .param("id", inv.snapshot.id.clone()),
    )
    .await
    .unwrap();
    assert!(repo::parent(&g, &parent, &inv.snapshot.id).await.is_err());
    g.run(
        query("MATCH(i:SchoolInvoice {id:$id}) SET i.student_id=$student")
            .param("id", inv.snapshot.id.clone())
            .param("student", student.clone()),
    )
    .await
    .unwrap();
    // Pre-policy v1 audits were also written only after the same current-family
    // eligibility guard. Missing new annotation never authorizes an unaudited row.
    g.run(query("MATCH(a:SchoolInvoiceAudit {invoice_id:$id,action:'issued'}) REMOVE a.eligibility_contract,a.payer_user_id,a.student_id,a.enrolled_student_id,a.school_id,a.tenant_id,a.request_hash").param("id",inv.snapshot.id.clone())).await.unwrap();
    g.run(
        query("MATCH(m:StaffMember {id:$staff}) SET m.schoolIds=['foreign-school']")
            .param("staff", finance.id.clone()),
    )
    .await
    .unwrap();
    assert!(matches!(
        repo::finance(&g, &finance, &inv.snapshot.id).await,
        Err(repo::Error::Denied)
    ));
    g.run(
        query("MATCH(m:StaffMember {id:$staff}) SET m.schoolIds=[$school]")
            .param("staff", finance.id.clone())
            .param("school", school.clone()),
    )
    .await
    .unwrap();
    g.run(
        query("MATCH(e:EnrolledStudent {student_id:$id}) SET e.status='graduated'")
            .param("id", enrolled.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        repo::parent(&g, &parent, &inv.snapshot.id)
            .await
            .unwrap()
            .snapshot
            .payer_user_id,
        payer
    );
    assert_eq!(repo::list_parent(&g, &parent).await.unwrap().len(), 1);
    assert!(matches!(
        repo::parent(
            &g,
            &Parent {
                subject: format!("foreign-parent-{mark}"),
                expires: parent.expires
            },
            &inv.snapshot.id
        )
        .await,
        Err(repo::Error::Denied)
    ));
    // Both the old guarded-v1 audit and the explicit new eligibility annotation
    // authorize only this original invoice after graduation.
    g.run(query("MATCH(i:SchoolInvoice {id:$id}),(a:SchoolInvoiceAudit {invoice_id:$id,action:'issued'}) SET a.eligibility_contract='sis-current-family-v1',a.payer_user_id=i.payer_user_id,a.student_id=i.student_id,a.enrolled_student_id=i.enrolled_student_id,a.school_id=i.school_id,a.tenant_id=i.tenant_id,a.request_hash=i.request_hash").param("id",inv.snapshot.id.clone())).await.unwrap();
    assert!(repo::parent(&g, &parent, &inv.snapshot.id).await.is_ok());
    assert!(matches!(
        repo::issue(&g, &finance, &input).await,
        Err(repo::Error::Denied)
    ));
    g.run(
        query("MATCH(e:EnrolledStudent {student_id:$id}) SET e.status='transferred'")
            .param("id", enrolled.clone()),
    )
    .await
    .unwrap();
    let p = Proof {
        id: format!("SPROOF-{}", uuid::Uuid::new_v4()),
        invoice_id: inv.snapshot.id.clone(),
        amount_submitted: 100,
        paid_at: "2026-10-04T12:00:00+07:00".into(),
        payer_name: "Synthetic Parent".into(),
        payer_bank: "Synthetic Bank".into(),
        reference_number: "No money transferred".into(),
        mime_type: "application/pdf".into(),
        file_name: "fixture.pdf".into(),
        size_bytes: 10,
        document_hash: mark.clone(),
        uploaded_at: chrono::Utc::now().to_rfc3339(),
    };
    let mut expiring_parent = parent.clone();
    expiring_parent.expires = chrono::Utc::now().timestamp() + 2;
    let held = g.start_txn().await.unwrap();
    held.run(
        query("MATCH(i:SchoolInvoice {id:$id}) SET i._lock=coalesce(i._lock,0)+1")
            .param("id", inv.snapshot.id.clone()),
    )
    .await
    .unwrap();
    let queued = repo::record_proof(&g, &expiring_parent, &inv, &p, "not-stored-expired");
    tokio::pin!(queued);
    tokio::select! {_=&mut queued=>panic!("proof did not wait"),_=tokio::time::sleep(std::time::Duration::from_secs(2))=>{}}
    held.commit().await.unwrap();
    assert!(matches!(queued.await, Err(repo::Error::Expired)));
    assert_eq!(
        repo::parent(&g, &parent, &inv.snapshot.id)
            .await
            .unwrap()
            .version,
        0
    );
    let submitted = repo::record_proof(&g, &parent, &inv, &p, "test-never-stored")
        .await
        .unwrap();
    assert_eq!(submitted.status, "pending_verification");
    assert_eq!(submitted.version, 1);
    assert!(matches!(
        repo::record_proof(&g, &parent, &inv, &p, "test-never-stored").await,
        Err(repo::Error::Conflict)
    ));
    let decision = Review {
        version: 1,
        proof_id: p.id.clone(),
        decision: "approve".into(),
        note: "Synthetic acceptance; no money transferred".into(),
    };
    // Staff suspension committed while review waits for Payment's lock is
    // observed by the fresh canonical guard after that wait.
    let held = g.start_txn().await.unwrap();
    held.run(
        query("MATCH(i:SchoolInvoice {id:$id}) SET i._lock=coalesce(i._lock,0)+1")
            .param("id", inv.snapshot.id.clone()),
    )
    .await
    .unwrap();
    let queued = repo::review(&g, &finance, &inv.snapshot.id, &decision);
    tokio::pin!(queued);
    tokio::select! {_=&mut queued=>panic!("review did not wait for the invoice lock"),_=tokio::time::sleep(std::time::Duration::from_millis(150))=>{}}
    g.run(
        query("MATCH(m:StaffMember {id:$id}) SET m.membershipStatus='SUSPENDED'")
            .param("id", finance.id.clone()),
    )
    .await
    .unwrap();
    held.commit().await.unwrap();
    assert!(queued.await.is_err());
    assert_eq!(
        repo::parent(&g, &parent, &inv.snapshot.id)
            .await
            .unwrap()
            .status,
        "pending_verification"
    );
    g.run(
        query("MATCH(m:StaffMember {id:$id}) SET m.membershipStatus='ACTIVE'")
            .param("id", finance.id.clone()),
    )
    .await
    .unwrap();
    // The selected proof lock must also precede the final live Staff/expiry guard.
    let mut expiring = finance.clone();
    expiring.expires = chrono::Utc::now().timestamp() + 2;
    let held = g.start_txn().await.unwrap();
    held.run(
        query("MATCH(p:SchoolInvoiceProof {id:$id}) SET p._lock=coalesce(p._lock,0)+1")
            .param("id", p.id.clone()),
    )
    .await
    .unwrap();
    let queued = repo::review(&g, &expiring, &inv.snapshot.id, &decision);
    tokio::pin!(queued);
    tokio::select! {_=&mut queued=>panic!("review did not wait for proof"),_=tokio::time::sleep(std::time::Duration::from_secs(2))=>{}}
    held.commit().await.unwrap();
    assert!(matches!(queued.await, Err(repo::Error::Expired)));
    assert_eq!(
        repo::finance(&g, &finance, &inv.snapshot.id)
            .await
            .unwrap()
            .version,
        1
    );
    // Rejection preserves evidence; corrected metadata may reuse the same file.
    let rejection = Review {
        decision: "reject".into(),
        version: decision.version,
        proof_id: decision.proof_id.clone(),
        note: decision.note.clone(),
    };
    let rejected = repo::review(&g, &finance, &inv.snapshot.id, &rejection)
        .await
        .unwrap();
    let revised = Proof {
        id: format!("SPROOF-{}", uuid::Uuid::new_v4()),
        reference_number: "Corrected reference".into(),
        ..p.clone()
    };
    let resubmitted = repo::record_proof(&g, &parent, &rejected, &revised, "test-never-stored-2")
        .await
        .unwrap();
    assert_eq!(resubmitted.version, 3);
    assert_eq!(repo::proofs(&g, &inv.snapshot.id).await.unwrap().len(), 2);
    let decision = Review {
        version: 3,
        proof_id: revised.id,
        decision: decision.decision.clone(),
        note: decision.note.clone(),
    };
    g.run(
        query("MATCH(e:EnrolledStudent {student_id:$id}) SET e.status='inactive'")
            .param("id", enrolled.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        repo::finance(&g, &finance, &inv.snapshot.id)
            .await
            .unwrap()
            .status,
        "pending_verification"
    );
    let (a, b) = tokio::join!(
        repo::review(&g, &finance, &inv.snapshot.id, &decision),
        repo::review(&g, &finance, &inv.snapshot.id, &decision)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        repo::parent(&g, &parent, &inv.snapshot.id)
            .await
            .unwrap()
            .snapshot
            .payer_user_id,
        payer
    );
    let paid = repo::finance(&g, &finance, &inv.snapshot.id).await.unwrap();
    assert_eq!(paid.status, "paid");
    assert_eq!(paid.version, 4);
    assert!(paid.paid_at.is_some());
    assert!(matches!(
        repo::record_proof(&g, &parent, &paid, &p, "not-stored-after-paid").await,
        Err(repo::Error::Conflict)
    ));
    assert!(super::documents::render(&paid, true)
        .unwrap()
        .starts_with(b"%PDF-"));
    let mut result=g.execute(query("MATCH(s:Student {studentId:$student}),(l:Lead {lead_id:$lead}) OPTIONAL MATCH(settlement:SchoolInvoiceSettlement {invoice_id:$invoice}) WITH s,l,count(settlement) AS settlements OPTIONAL MATCH(a:SchoolInvoiceAudit {invoice_id:$invoice,action:'issued'}) RETURN s.applicantStatus AS student_status,l.status AS lead_status,settlements,count(a) AS issuance_count").param("student",student).param("lead",format!("LEAD-{mark}")).param("invoice",inv.snapshot.id.clone())).await.unwrap();
    let row = result.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>("student_status").unwrap(),
        "handed_to_sis"
    );
    assert_eq!(row.get::<String>("lead_status").unwrap(), "handed_to_sis");
    assert_eq!(row.get::<i64>("settlements").unwrap(), 1);
    assert_eq!(row.get::<i64>("issuance_count").unwrap(), 1);
    g.run(query("MATCH(u:User {id:$payer}) SET u.role='teacher'").param("payer", payer.clone()))
        .await
        .unwrap();
    assert!(matches!(
        repo::parent(&g, &parent, &inv.snapshot.id).await,
        Err(repo::Error::Denied)
    ));
    // Clean only this test's newly created nodes, not any retained acceptance graph.
    g.run(query("MATCH(n) WHERE n.billing_test=$mark OR (n:SchoolInvoice AND n.tenant_id=$tenant) OR (n:SchoolBillingPayee AND n.tenant_id=$tenant) OR ((n:SchoolInvoiceAudit OR n:SchoolInvoiceProof OR n:SchoolInvoiceSettlement) AND (n.invoice_id=$invoice OR n.tenant_id=$tenant)) DETACH DELETE n").param("mark",mark).param("tenant",tenant).param("invoice",inv.snapshot.id.clone())).await.unwrap();
}

#[tokio::test]
#[ignore = "requires explicitly owned disposable SCHOOL_INVOICE_TEST_NEO4J_URI"]
async fn configuration_authority_is_independent_of_historical_invoice_bounds() {
    let uri = std::env::var("SCHOOL_INVOICE_TEST_NEO4J_URI").unwrap();
    assert!(uri.starts_with("bolt://127.0.0.1:"));
    let g = Graph::new(
        &uri,
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_USER").unwrap(),
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_PASSWORD").unwrap(),
    )
    .await
    .unwrap();
    let mark = uuid::Uuid::new_v4().to_string();
    let tenant = format!("bound-tenant-{mark}");
    let school = format!("bound-school-{mark}");
    let actor = Staff {
        subject: format!("bound-user-{mark}"),
        id: format!("bound-staff-{mark}"),
        expires: chrono::Utc::now().timestamp() + 900,
    };
    let bank=json!([{"id":"bank-fixture","bankName":"Synthetic Bank","accountName":"Synthetic School","accountNumber":"0000000","instructions":"No money transfer","enabled":true}]).to_string();
    g.run(query("CREATE(:User {id:$user,billing_test:$mark})-[:STAFF_MEMBER]->(:StaffMember {id:$staff,membershipStatus:'ACTIVE',roles:['owner'],schoolIds:[$school],tenantIds:[$tenant],teamIds:[],billing_test:$mark}) CREATE(:School {school_id:$school,tenant_id:$tenant,name:'Synthetic School',billing_test:$mark}) CREATE(:PaymentSettings {tenant_id:$tenant,manual_transfer_enabled:true,manual_bank_accounts_json:$bank,billing_test:$mark}) WITH 1 AS ignored UNWIND range(1,201) AS n CREATE(:SchoolInvoice {id:'SINV-bound-'+$mark+'-'+toString(n),tenant_id:$tenant,school_id:$school,snapshot:'malformed-historical-json',status:'pending',version:0,billing_test:$mark})").param("user",actor.subject.clone()).param("staff",actor.id.clone()).param("school",school.clone()).param("tenant",tenant.clone()).param("bank",bank).param("mark",mark.clone())).await.unwrap();
    assert!(matches!(
        repo::list_finance(&g, &actor).await,
        Err(repo::Error::Unavailable)
    ));
    let result = payee::list(&g, &actor).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].school_id, school);
    assert_eq!(result[0].banks.len(), 1);
    assert!(result[0].bank.is_none());
    assert_eq!(result[0].version, 0);
    let foreign = Staff {
        subject: "foreign-user".into(),
        id: "foreign-staff".into(),
        expires: actor.expires,
    };
    assert!(matches!(
        payee::list(&g, &foreign).await,
        Err(repo::Error::Denied)
    ));
    g.run(query("MATCH(n {billing_test:$mark}) DETACH DELETE n").param("mark", mark))
        .await
        .unwrap();
}
