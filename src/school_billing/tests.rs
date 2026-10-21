use super::{checkout, model::*, repository as repo, service, worker};
use crate::{
    clients::doku::{DokuWebhook, DokuWebhookOrder, DokuWebhookTransaction},
    school_invoice::{
        auth::{Parent, Staff},
        payee,
        repository::{self as invoices, Error},
    },
};
use chrono::Datelike;
use neo4rs::{query, Graph};
use serde_json::json;

fn inst(label: &str, amount: i64, due: &str) -> Installment {
    Installment {
        label: label.into(),
        amount,
        due_date: due.into(),
    }
}
fn paid(number: &str, amount: i64, request: &str) -> DokuWebhook {
    DokuWebhook {
        order: DokuWebhookOrder {
            invoice_number: number.into(),
            amount: amount.into(),
            currency: "IDR".into(),
            status: Some("SUCCESS".into()),
        },
        transaction: Some(DokuWebhookTransaction {
            status: "SUCCESS".into(),
            original_request_id: Some(request.into()),
            date: None,
        }),
        service: None,
        channel: None,
    }
}

#[tokio::test]
#[ignore = "requires explicitly owned disposable SCHOOL_INVOICE_TEST_NEO4J_URI"]
async fn monthly_levy_overseas_credit_and_online_settlement() {
    let uri = std::env::var("SCHOOL_INVOICE_TEST_NEO4J_URI").expect("explicit test graph");
    assert!(uri.starts_with("bolt://127.0.0.1:"));
    let g = Graph::new(
        &uri,
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_USER").unwrap(),
        &std::env::var("SCHOOL_INVOICE_TEST_NEO4J_PASSWORD").unwrap(),
    )
    .await
    .unwrap();
    invoices::init(&g).await.unwrap();
    repo::init(&g).await.unwrap();
    let mark = uuid::Uuid::new_v4().simple().to_string();
    let tenant = format!("bt-{mark}");
    let school = format!("bs-{mark}");
    let student = format!("bc-{mark}");
    let enrolled = format!("be-{mark}");
    let payer = format!("bp-{mark}");
    let exp = chrono::Utc::now().timestamp() + 900;
    let owner = Staff {
        subject: format!("bo-{mark}"),
        id: format!("bos-{mark}"),
        expires: exp,
    };
    let parent = Parent {
        subject: payer.clone(),
        expires: exp,
    };
    let stranger = Parent {
        subject: format!("bx-{mark}"),
        expires: exp,
    };
    let bank=json!([{"id":"bank-fixture","bankName":"Synthetic Bank","accountName":"Synthetic School","accountNumber":"0000000000","instructions":"Fixture, do not transfer money","enabled":true}]).to_string();
    g.run(query("CREATE(u:User {id:$payer,role:'parent',email:$email,fullName:'Synthetic Parent',billing_test:$mark})-[:HAS_APPLICATION]->(l:Lead {lead_id:$lead,email:$email,otp_verified:true,billing_test:$mark})-[:HAS_STUDENT]->(s:Student {studentId:$student,fullName:'Synthetic Child',billing_test:$mark}) CREATE(s)-[:ENROLLED_AS]->(e:EnrolledStudent {student_id:$enrolled,applicant_student_id:$student,status:'active',school_id:$school,tenant_id:$tenant,billing_test:$mark})-[:ENROLLED_IN]->(:Section {section_id:$section,status:'active',year_group:'10',school_id:$school,tenant_id:$tenant,billing_test:$mark}) CREATE(:School {school_id:$school,school_code:'IISS',tenant_id:$tenant,name:'Synthetic School',billing_test:$mark}) CREATE(:PaymentSettings {tenant_id:$tenant,manual_transfer_enabled:true,manual_bank_accounts_json:$bank,billing_test:$mark}) CREATE(:User {id:$owner,billing_test:$mark})-[:STAFF_MEMBER]->(:StaffMember {id:$ownerstaff,membershipStatus:'ACTIVE',roles:['owner'],tenantIds:[$tenant],schoolIds:[$school],teamIds:[],billing_test:$mark}) CREATE(:User {id:$stranger,role:'parent',billing_test:$mark})")
        .param("payer",payer.clone()).param("email",format!("b-{mark}@example.test")).param("lead",format!("LEAD-{mark}")).param("student",student.clone()).param("enrolled",enrolled.clone()).param("section",format!("bsec-{mark}")).param("tenant",tenant.clone()).param("school",school.clone()).param("owner",owner.subject.clone()).param("ownerstaff",owner.id.clone()).param("stranger",stranger.subject.clone()).param("bank",bank).param("mark",mark.clone())).await.unwrap();

    // Without a payee nothing can be billed.
    let c = repo::family(&g, &payer, &student).await.unwrap();
    assert_eq!(
        (c.enrolled_student_id.as_str(), c.year_group.as_str()),
        (enrolled.as_str(), "10")
    );
    assert!(matches!(
        repo::family(&g, &stranger.subject, &student).await,
        Err(Error::Denied)
    ));
    assert!(repo::children(&g, &stranger).await.unwrap().is_empty());
    payee::configure(
        &g,
        &owner,
        &payee::Configure {
            tenant_id: tenant.clone(),
            school_id: school.clone(),
            bank_account_id: "bank-fixture".into(),
            version: 0,
        },
    )
    .await
    .unwrap();

    // ---- Advance Monthly Fee
    let plan = FeePlanInput {
        tenant_id: tenant.clone(),
        school_id: school.clone(),
        year_group: "*".into(),
        monthly_amount: 7_000_000,
        options: vec![
            PeriodOption {
                months: 3,
                discount_bps: 0,
            },
            PeriodOption {
                months: 6,
                discount_bps: 300,
            },
        ],
        due_day: 10,
        issue_lead_days: 14,
        active: true,
        version: 0,
    };
    assert_eq!(
        repo::save_plan(&g, &owner, &plan)
            .await
            .unwrap()
            .input
            .version,
        1
    );
    assert!(
        matches!(
            repo::save_plan(&g, &owner, &plan).await,
            Err(Error::Conflict)
        ),
        "stale plan version"
    );
    assert!(
        service::issue_monthly(&g, &c, today())
            .await
            .unwrap()
            .is_none(),
        "no period chosen yet"
    );
    repo::select(&g, &parent, &c, 6, 0).await.unwrap();
    let first = service::issue_monthly(&g, &c, today())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (first.amount, first.discount, first.months),
        (40_740_000, 1_260_000, 6)
    );
    assert_eq!(first.period_start, Month::of(today()).key());
    assert!(
        service::issue_monthly(&g, &c, today())
            .await
            .unwrap()
            .is_none(),
        "open period blocks the next"
    );
    assert!(
        matches!(
            repo::select(&g, &parent, &c, 3, 1).await,
            Err(Error::Conflict)
        ),
        "locked while open"
    );

    // ---- Online Payment settles once; a mismatched amount never does.
    let number = format!("{}-1", first.invoice_id);
    g.run(query("MATCH (i:SchoolInvoice {id:$id}) CREATE (i)-[:PAID_ONLINE_VIA]->(:SchoolInvoiceCheckout {invoice_number:$n,invoice_id:i.id,request_id:'req-1',amount:$a,currency:'IDR',status:'created',url:'https://example.test',created_at:datetime(),expires_at:datetime()+duration('PT1H')})").param("id",first.invoice_id.clone()).param("n",number.clone()).param("a",first.amount)).await.unwrap();
    assert!(
        checkout::apply(&g, &paid(&number, first.amount - 1, "req-1"))
            .await
            .is_err()
    );
    assert!(
        checkout::apply(&g, &paid(&number, first.amount, "req-other"))
            .await
            .is_err()
    );
    assert!(checkout::apply(&g, &paid(&number, first.amount, "req-1"))
        .await
        .unwrap());
    assert!(
        !checkout::apply(&g, &paid(&number, first.amount, "req-1"))
            .await
            .unwrap(),
        "replay is a no-op"
    );
    assert_eq!(
        invoices::parent(&g, &parent, &first.invoice_id)
            .await
            .unwrap()
            .status,
        "paid"
    );
    assert!(matches!(
        invoices::parent(&g, &stranger, &first.invoice_id).await,
        Err(Error::Denied)
    ));
    // Paid: the period may change, and the next one waits for its window.
    assert_eq!(
        repo::select(&g, &parent, &c, 3, 1).await.unwrap().version,
        2
    );
    let start = Month::of(today()).plus(6);
    let early = start.day(10) - chrono::Duration::days(15);
    assert!(service::issue_monthly(&g, &c, early)
        .await
        .unwrap()
        .is_none());
    let next = service::issue_monthly(&g, &c, early + chrono::Duration::days(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((next.period_start, next.amount), (start.key(), 21_000_000));

    // ---- Capital Levy: the year of entry is the admissions offer's; later years come from plans.
    g.run(query("MATCH (s:Student {studentId:$student}) CREATE (s)-[:HAS_OFFER]->(o:Offer {offer_id:$offer,status:'accepted',academic_year:'2026',tenant_id:$tenant,target_school_id:$school,issued_at:datetime(),pricing_snapshot_json:$pricing,billing_test:$mark})-[:PAID_VIA]->(:Payment {payment_id:$pay,payment_type:'offer_due_now',status:'paid',amount:105000000,billing_test:$mark})")
        .param("student", student.clone()).param("offer", format!("OFF-{mark}")).param("pay", format!("PAY-{mark}")).param("tenant", tenant.clone()).param("school", school.clone()).param("mark", mark.clone())
        .param("pricing", r#"{"snapshotVersion":"offer-pricing-v1","currency":"IDR","lineItems":[{"code":"CAPITAL_LEVY","label":"Capital Levy","amount":75000000},{"code":"MONTHLY_FEE","label":"Monthly Fee - July 2099","amount":7000000}],"remainingBalance":0}"#)).await.unwrap();
    let admitted = repo::admission_paid(&g, &c).await.unwrap().unwrap();
    assert_eq!(
        (admitted.academic_year.as_str(), admitted.capital_levy),
        ("2026", Some(75_000_000))
    );
    // The offer's monthly fee is July of its academic year, whatever its label says.
    assert_eq!(
        admitted.monthly_fee_month.map(Month::key),
        Some("2026-07".into())
    );
    let plan = |year: &str, version: i64| CapitalLevyPlanInput {
        tenant_id: tenant.clone(),
        school_id: school.clone(),
        year_group: "*".into(),
        academic_year: year.into(),
        installments: vec![
            inst("Capital Levy I", 75_000_000, "2026-01-10"),
            inst("Capital Levy II", 75_000_000, "2099-01-10"),
        ],
        issue_lead_days: 14,
        active: true,
        version,
    };
    repo::save_levy_plan(&g, &owner, &plan("2026", 0))
        .await
        .unwrap();
    service::ensure_levies(&g, &c).await.unwrap();
    assert!(
        repo::capital_levies_for(&g, &c).await.unwrap().is_empty(),
        "the year of entry was paid in admissions"
    );
    repo::save_levy_plan(&g, &owner, &plan("2027", 0))
        .await
        .unwrap();
    service::ensure_levies(&g, &c).await.unwrap();
    let saved = repo::capital_levies_for(&g, &c).await.unwrap().remove(0);
    assert_eq!(
        (saved.academic_year.as_str(), saved.payable),
        ("2027", 150_000_000)
    );
    let i1 = service::issue_installment(&g, &c, &saved, today())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((i1.amount, i1.installment), (75_000_000, 0));
    assert!(
        service::issue_installment(&g, &c, &saved, today())
            .await
            .unwrap()
            .is_none(),
        "II is not due"
    );
    // Once billed, a changed plan no longer rewrites the child's schedule.
    let mut changed = plan("2027", 1);
    changed.installments[1].amount = 1;
    repo::save_levy_plan(&g, &owner, &changed).await.unwrap();
    service::ensure_levies(&g, &c).await.unwrap();
    assert_eq!(
        repo::capital_levies_for(&g, &c).await.unwrap()[0].payable,
        150_000_000
    );
    assert!(
        service::deposit(&g, &c, &saved, 75_000_001, "dep-1")
            .await
            .is_err(),
        "over the unbilled remainder"
    );
    let d1 = service::deposit(&g, &c, &saved, 50_000_000, "dep-1")
        .await
        .unwrap();
    assert_eq!(
        service::deposit(&g, &c, &saved, 50_000_000, "dep-1")
            .await
            .unwrap()
            .invoice_id,
        d1.invoice_id,
        "same request, same invoice"
    );
    let late = chrono::NaiveDate::from_ymd_opt(2099, 1, 1).unwrap();
    let i2 = service::issue_installment(&g, &c, &saved, late)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(i2.amount, 25_000_000, "deposit shrank installment II");

    // ---- Overseas: terms version, quota, scheme, discount caps billing.
    let program = OverseasProgramInput {
        tenant_id: tenant.clone(),
        school_id: school.clone(),
        title: "Overseas 2027".into(),
        summary: "Study trip".into(),
        terms: "1. Definisi".into(),
        year_groups: vec!["10".into()],
        quota: 1,
        early_bird_until: Some("2099-12-31".into()),
        early_bird_discount: 15_000_000,
        early_bird_installments: vec![
            inst("Cicilan 1", 50_000_000, "2026-01-12"),
            inst("Cicilan 2", 75_000_000, "2099-03-12"),
        ],
        installments: vec![inst("Cicilan 1", 125_000_000, "2026-01-12")],
        issue_lead_days: 14,
        status: "open".into(),
        version: 0,
    };
    let p = repo::save_program(&g, &owner, None, &program)
        .await
        .unwrap();
    assert!(matches!(
        repo::enroll(&g, &parent, &c, &p, "stale-terms", today()).await,
        Err(Error::Conflict)
    ));
    let e = repo::enroll(&g, &parent, &c, &p, &p.terms_version, today())
        .await
        .unwrap();
    assert_eq!((e.scheme.as_str(), e.payable), ("early_bird", 110_000_000));
    assert!(
        matches!(
            repo::enroll(&g, &parent, &c, &p, &p.terms_version, today()).await,
            Err(Error::Conflict)
        ),
        "once per child"
    );
    assert_eq!(
        service::issue_installment(&g, &c, &e, today())
            .await
            .unwrap()
            .unwrap()
            .amount,
        50_000_000
    );
    let o2 = service::issue_installment(&g, &c, &e, late.with_month(3).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        o2.amount, 60_000_000,
        "discount comes off the last installment"
    );

    // ---- Student Credit
    assert!(service::top_up(&g, &c, 1, "top-1").await.is_err());
    let t = service::top_up(&g, &c, 500_000, "top-1").await.unwrap();
    let tn = format!("{}-1", t.invoice_id);
    g.run(query("MATCH (i:SchoolInvoice {id:$id}) CREATE (i)-[:PAID_ONLINE_VIA]->(:SchoolInvoiceCheckout {invoice_number:$n,invoice_id:i.id,request_id:'req-2',amount:500000,currency:'IDR',status:'created',url:'https://example.test',created_at:datetime(),expires_at:datetime()+duration('PT1H')})").param("id",t.invoice_id.clone()).param("n",tn.clone())).await.unwrap();
    assert!(checkout::apply(&g, &paid(&tn, 500_000, "req-2"))
        .await
        .unwrap());

    // Three open top-ups at most; a fourth is refused.
    for r in ["top-2", "top-3", "top-4"] {
        service::top_up(&g, &c, 100_000, r).await.unwrap();
    }
    assert!(service::top_up(&g, &c, 100_000, "top-5").await.is_err());

    // ---- An online payment never settles a bill whose transfer proof is under review.
    let review = service::deposit(&g, &c, &saved, 100_000, "dep-review").await;
    assert!(review.is_err(), "levy fully billed by now");
    let open_levy = i2.invoice_id.clone();
    g.run(query("MATCH (i:SchoolInvoice {id:$id}) SET i.status='pending_verification' CREATE (i)-[:PAID_ONLINE_VIA]->(:SchoolInvoiceCheckout {invoice_number:$n,invoice_id:i.id,request_id:'req-3',amount:$a,currency:'IDR',status:'created',url:'https://example.test',created_at:datetime(),expires_at:datetime()+duration('PT1H')})").param("id",open_levy.clone()).param("n",format!("{open_levy}-1")).param("a",i2.amount)).await.unwrap();
    assert!(
        !checkout::apply(&g, &paid(&format!("{open_levy}-1"), i2.amount, "req-3"))
            .await
            .unwrap()
    );
    assert_eq!(
        invoices::parent(&g, &parent, &open_levy)
            .await
            .unwrap()
            .status,
        "pending_verification"
    );
    assert_eq!(
        status(&g, format!("{open_levy}-1")).await,
        "paid_needs_reconciliation"
    );
    // A second order paid after the bill settled is kept for a refund, once.
    g.run(query("MATCH (i:SchoolInvoice {id:$id}) CREATE (i)-[:PAID_ONLINE_VIA]->(:SchoolInvoiceCheckout {invoice_number:$n,invoice_id:i.id,request_id:'req-4',amount:$a,currency:'IDR',status:'created',url:'https://example.test',created_at:datetime(),expires_at:datetime()+duration('PT1H')})").param("id",first.invoice_id.clone()).param("n",format!("{}-2", first.invoice_id)).param("a",first.amount)).await.unwrap();
    assert!(!checkout::apply(
        &g,
        &paid(&format!("{}-2", first.invoice_id), first.amount, "req-4")
    )
    .await
    .unwrap());
    assert_eq!(
        status(&g, format!("{}-2", first.invoice_id)).await,
        "paid_duplicate"
    );
    assert!(!checkout::apply(
        &g,
        &paid(&format!("{}-2", first.invoice_id), first.amount, "req-4")
    )
    .await
    .unwrap());

    // ---- A second guardian of the same child neither overwrites the period nor re-bills months.
    let other = format!("bq-{mark}");
    g.run(query("MATCH (s:Student {studentId:$student}) CREATE (u:User {id:$other,role:'parent',email:$email,billing_test:$mark})-[:HAS_APPLICATION]->(:Lead {lead_id:$lead,email:$email,otp_verified:true,billing_test:$mark})-[:HAS_STUDENT]->(s)").param("student",student.clone()).param("other",other.clone()).param("email",format!("bq-{mark}@example.test")).param("lead",format!("LEAD-q-{mark}")).param("mark",mark.clone())).await.unwrap();
    let second = Parent {
        subject: other.clone(),
        expires: exp,
    };
    let c_other = repo::family(&g, &other, &student).await.unwrap();
    assert!(matches!(
        repo::select(&g, &second, &c_other, 6, 2).await,
        Err(Error::Conflict)
    ));
    assert!(
        service::issue_monthly(&g, &c_other, today())
            .await
            .unwrap()
            .is_none(),
        "months already billed to the first guardian"
    );

    // ---- One live online order per bill; a failed one can be retried with the next number.
    let next_bill = service::top_up(&g, &c, 100_000, "top-reserve").await;
    assert!(next_bill.is_err(), "top-up cap still holds");
    let target = i2.invoice_id.clone();
    g.run(
        query("MATCH (i:SchoolInvoice {id:$id}) SET i.status='rejected'")
            .param("id", target.clone()),
    )
    .await
    .unwrap();
    let n2 = checkout::reserve(&g, &target, i2.amount, "req-r1", &parent)
        .await
        .unwrap();
    assert_eq!(n2, format!("{target}-2"));
    assert!(matches!(
        checkout::reserve(&g, &target, i2.amount, "req-r2", &parent).await,
        Err(Error::Conflict)
    ));
    g.run(
        query("MATCH (c:SchoolInvoiceCheckout {invoice_number:$n}) SET c.status='failed'")
            .param("n", n2),
    )
    .await
    .unwrap();
    assert_eq!(
        checkout::reserve(&g, &target, i2.amount, "req-r3", &parent)
            .await
            .unwrap(),
        format!("{target}-3")
    );

    // ---- Summary as the Payment tab reads it.
    let view = service::summary(&g, c.clone()).await.unwrap();
    let v = serde_json::to_value(&view).unwrap();
    assert_eq!(v["credit"]["balance"], 500_000);
    assert_eq!(v["credit"]["bills"].as_array().unwrap().len(), 4);
    assert_eq!(v["monthlyFee"]["locked"], true);
    assert_eq!(v["monthlyFee"]["selection"]["months"], 3);
    assert_eq!(v["capitalLevy"]["billed"], 150_000_000);
    assert_eq!(v["capitalLevy"]["paid"], 0);
    assert_eq!(v["capitalLevy"]["academicYear"], "2027");
    assert_eq!(v["capitalLevyYears"][1]["source"], "admission");
    assert_eq!(v["capitalLevyYears"][1]["paid"], 75_000_000);
    assert_eq!(v["overseas"][0]["enrollment"]["payable"], 110_000_000);
    assert_eq!(v["overseas"][0]["participants"], 1);
    assert!(
        v.get("payerUserId").is_none(),
        "payer id never leaves the service"
    );

    // ---- The worker finds nothing new to issue twice.
    let before = repo::bills(&g, &payer, &enrolled).await.unwrap().len();
    let _ = worker::run_once(&g).await.unwrap();
    let _ = worker::run_once(&g).await.unwrap();
    let after = repo::bills(&g, &payer, &enrolled).await.unwrap().len();
    assert!(
        after <= before + 1,
        "at most one Overseas installment II style catch-up"
    );

    g.run(query("MATCH (n) WHERE n.billing_test=$mark OR n.tenant_id=$tenant OR n.invoice_id STARTS WITH 'SINV-' AND EXISTS { MATCH (i:SchoolInvoice {id:n.invoice_id,tenant_id:$tenant}) } DETACH DELETE n").param("mark",mark).param("tenant",tenant)).await.unwrap();
}

async fn status(g: &Graph, n: String) -> String {
    g.execute(
        query("MATCH (c:SchoolInvoiceCheckout {invoice_number:$n}) RETURN c.status AS s")
            .param("n", n),
    )
    .await
    .unwrap()
    .next()
    .await
    .unwrap()
    .unwrap()
    .get::<String>("s")
    .unwrap()
}
