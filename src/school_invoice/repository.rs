use super::{auth, model::*};
use neo4rs::{query, Graph, Query, Row};
use sha2::{Digest, Sha256};

// This is the actual SIS-owned read contract. Only the shared-graph read is
// reused, so Payment writes its own labels and audit/ledger exclusively.
const FAMILY: &str = include_str!("contracts/sis-current-family-v1.cypher");
#[derive(Debug)]
pub enum Error {
    Unavailable,
    Expired,
    Denied,
    Conflict,
}
pub type Result<T> = std::result::Result<T, Error>;
fn unavailable(_: impl std::fmt::Debug) -> Error {
    Error::Unavailable
}
/// No failed guard/CAS or parse may commit a draft invoice or lock metadata.
pub(super) async fn write<T>(
    graph: &Graph,
    q: Query,
    expires: i64,
    parse: impl FnOnce(&Row) -> Result<T>,
) -> Result<T> {
    valid_time(expires)?;
    let tx = graph.start_txn().await.map_err(unavailable)?;
    let result = async {
        let mut rows = tx.execute(q).await.map_err(unavailable)?;
        let row = rows.next().await.map_err(unavailable)?.ok_or_else(|| {
            if auth::live(expires) {
                Error::Conflict
            } else {
                Error::Expired
            }
        })?;
        let result = parse(&row)?;
        if rows.next().await.map_err(unavailable)?.is_some() {
            return Err(Error::Unavailable);
        }
        valid_time(expires)?;
        Ok(result)
    }
    .await;
    match result {
        Ok(value) => {
            tx.commit().await.map_err(unavailable)?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await.map_err(unavailable)?;
            Err(error)
        }
    }
}
fn valid_time(expires: i64) -> Result<()> {
    if auth::live(expires) {
        Ok(())
    } else {
        Err(Error::Expired)
    }
}
fn invoice(row: &Row) -> Result<Invoice> {
    let raw = row.get::<String>("snapshot").ok_or(Error::Unavailable)?;
    let snapshot: Snapshot = serde_json::from_str(&raw).map_err(unavailable)?;
    // Bind the immutable wire snapshot to the exact graph selectors used for
    // authorization, and to the original issuance command (including child).
    for (key, value) in [
        ("id", &snapshot.id),
        ("tenant", &snapshot.tenant_id),
        ("school", &snapshot.school_id),
        ("student", &snapshot.student_id),
        ("enrolled", &snapshot.enrolled_student_id),
        ("payer", &snapshot.payer_user_id),
    ] {
        if row.get::<String>(key).as_ref() != Some(value) {
            return Err(Error::Unavailable);
        }
    }
    if !snapshot.matches_issuance(
        &row.get::<String>("issue_key").ok_or(Error::Unavailable)?,
        &row.get::<String>("request_hash")
            .ok_or(Error::Unavailable)?,
    ) {
        return Err(Error::Unavailable);
    }
    Ok(Invoice {
        snapshot,
        kind: "school_invoice".into(),
        status: row.get("status").ok_or(Error::Unavailable)?,
        version: row.get("version").ok_or(Error::Unavailable)?,
        paid_at: row.get("paid"),
        receipt_ref: row.get("receipt"),
    })
}
const RETURN:&str="RETURN i.snapshot AS snapshot,i.status AS status,i.version AS version,i.paid_at AS paid,i.receipt_ref AS receipt,i.id AS id,i.tenant_id AS tenant,i.school_id AS school,i.student_id AS student,i.enrolled_student_id AS enrolled,i.payer_user_id AS payer,i.issue_key AS issue_key,i.request_hash AS request_hash";
fn staff_query(q: Query, a: &auth::Staff, review: bool) -> Query {
    q.param("subject", a.subject.clone())
        .param("staff", a.id.clone())
        .param("roles", auth::roles(review))
        .param("expires", a.expires)
}
fn parent_guard() -> String {
    super::parent_authority::guard()
}
fn finance_prefix() -> String {
    format!(
        "{} WITH actor MATCH (i:SchoolInvoice) WHERE {} ",
        auth::STAFF,
        auth::scope("i.school_id", "i.tenant_id")
    )
}

pub async fn init(graph: &Graph) -> Result<()> {
    for (name, label, prop) in [
        ("school_invoice_id", "SchoolInvoice", "id"),
        ("school_invoice_issue_key", "SchoolInvoice", "issue_key"),
        ("school_invoice_proof_key", "SchoolInvoiceProof", "key"),
        ("school_billing_payee", "SchoolBillingPayee", "key"),
        (
            "school_invoice_settlement",
            "SchoolInvoiceSettlement",
            "invoice_id",
        ),
    ] {
        graph
            .run(query(&format!(
                "CREATE CONSTRAINT {name} IF NOT EXISTS FOR (n:{label}) REQUIRE n.{prop} IS UNIQUE"
            )))
            .await
            .map_err(unavailable)?;
    }
    Ok(())
}
pub(super) async fn schema_ready(graph: &Graph) -> Result<()> {
    let mut rows = graph
        .execute(query(
            "SHOW CONSTRAINTS YIELD name RETURN collect(name) AS names",
        ))
        .await
        .map_err(unavailable)?;
    let names: Vec<String> = rows
        .next()
        .await
        .map_err(unavailable)?
        .ok_or(Error::Unavailable)?
        .get("names")
        .ok_or(Error::Unavailable)?;
    if [
        "school_invoice_id",
        "school_invoice_issue_key",
        "school_invoice_proof_key",
        "school_billing_payee",
        "school_invoice_settlement",
    ]
    .iter()
    .all(|n| names.iter().any(|x| x == n))
    {
        Ok(())
    } else {
        Err(Error::Unavailable)
    }
}
pub async fn issue(graph: &Graph, a: &auth::Staff, input: &Issue) -> Result<Invoice> {
    valid_time(a.expires)?;
    schema_ready(graph).await?;
    let prefix=format!("{} WITH actor {FAMILY} WHERE {} RETURN u.id AS payer,coalesce(u.fullName,u.name,u.email) AS payer_name,s.fullName AS student_name,e.student_id AS enrolled,e.school_id AS school,e.tenant_id AS tenant,coalesce(school.school_code,school.school_id) AS code,coalesce(school.name,school.school_name,school.school_code) AS payee",auth::STAFF,auth::scope("e.school_id","e.tenant_id"));
    // FAMILY's WITH clauses intentionally carry actor only for Finance queries.
    let prefix = carry_actor(&prefix);
    let mut rows = graph
        .execute(
            staff_query(query(&prefix), a, false)
                .param("payer", input.payer_user_id.clone())
                .param("student", input.student_id.clone()),
        )
        .await
        .map_err(unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(unavailable)?
        .ok_or(Error::Denied)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(Error::Denied);
    }
    let field = |k| row.get::<String>(k).ok_or(Error::Unavailable);
    let tenant = field("tenant")?;
    let mut banks=graph.execute(query("MATCH (config:SchoolBillingPayee {tenant_id:$tenant,school_id:$school,active:true,bank_account_id:$bank}) MATCH(ps:PaymentSettings {tenant_id:config.tenant_id,manual_transfer_enabled:true}) RETURN config.bank_json AS banks,config.version AS version,ps.manual_bank_accounts_json AS directory LIMIT 2").param("tenant",tenant.clone()).param("school",field("school")?).param("bank",input.bank_account_id.clone())).await.map_err(unavailable)?;
    let bankrow = banks
        .next()
        .await
        .map_err(unavailable)?
        .ok_or(Error::Denied)?;
    if banks.next().await.map_err(unavailable)?.is_some() {
        return Err(Error::Denied);
    }
    let bankraw = bankrow.get::<String>("banks").ok_or(Error::Unavailable)?;
    let payee_version = bankrow.get::<i64>("version").ok_or(Error::Unavailable)?;
    let directoryraw = bankrow
        .get::<String>("directory")
        .ok_or(Error::Unavailable)?;
    let directory: Vec<crate::repositories::payment_settings_repository::ManualBankAccount> =
        serde_json::from_str(&directoryraw).map_err(unavailable)?;
    let accounts: Vec<crate::repositories::payment_settings_repository::ManualBankAccount> =
        vec![serde_json::from_str(&bankraw).map_err(unavailable)?];
    let matches: Vec<_> = accounts
        .into_iter()
        .filter(|b| b.id == input.bank_account_id && b.enabled)
        .collect();
    if matches.len() != 1 {
        return Err(Error::Denied);
    }
    let bank = matches.into_iter().next().ok_or(Error::Denied)?;
    if directory
        .iter()
        .filter(|b| b.id == input.bank_account_id)
        .count()
        != 1
        || !directory.iter().any(|b| b == &bank && b.enabled)
    {
        return Err(Error::Denied);
    }
    if !text(&bank.bank_name, 128)
        || !text(&bank.account_name, 128)
        || !text(&bank.account_number, 64)
        || bank.instructions.len() > 1024
    {
        return Err(Error::Unavailable);
    }
    let snapshot = Snapshot {
        contract_version: 1,
        id: format!("SINV-{}", uuid::Uuid::new_v4()),
        tenant_id: tenant,
        school_id: field("school")?,
        school_code: field("code")?,
        payee_name: field("payee")?,
        student_id: input.student_id.clone(),
        enrolled_student_id: field("enrolled")?,
        student_name: field("student_name")?,
        payer_user_id: input.payer_user_id.clone(),
        payer_name: field("payer_name")?,
        period: input.period.clone(),
        description: input.description.clone(),
        amount: input.amount,
        currency: input.currency.clone(),
        due_date: input.due_date.clone(),
        bank,
        payee_version,
        issued_at: chrono::Utc::now().to_rfc3339(),
    };
    if [
        &snapshot.enrolled_student_id,
        &snapshot.school_id,
        &snapshot.tenant_id,
    ]
    .iter()
    .any(|v| !identifier(v))
        || [
            &snapshot.payer_name,
            &snapshot.student_name,
            &snapshot.payee_name,
        ]
        .iter()
        .any(|v| !text(v, 256))
    {
        return Err(Error::Unavailable);
    }
    let raw = serde_json::to_string(&snapshot).map_err(unavailable)?;
    let hash = hex::encode(Sha256::digest(
        serde_json::to_vec(input).map_err(unavailable)?,
    ));
    let issuekey = format!(
        "{}|{}|{}",
        snapshot.tenant_id, snapshot.school_id, input.idempotency_key
    );
    let body=format!("MATCH(config:SchoolBillingPayee {{tenant_id:$tenant,school_id:$school}}),(ps:PaymentSettings {{tenant_id:$tenant}}) SET ps.school_billing_lock=coalesce(ps.school_billing_lock,0)+1 SET config._lock=coalesce(config._lock,0)+1 WITH config,ps MERGE(i:SchoolInvoice {{issue_key:$key}}) ON CREATE SET i.id=$id SET i._lock=coalesce(i._lock,0)+1 WITH i,config,ps {} WITH i,config,ps,actor {} WHERE {} AND e.student_id=$enrolled AND e.school_id=$school AND e.tenant_id=$tenant AND config.school_id=e.school_id AND config.tenant_id=e.tenant_id AND config.active=true AND config.bank_json=$bankraw AND config.version=$payee_version AND ps.tenant_id=e.tenant_id AND ps.manual_transfer_enabled=true AND ps.manual_bank_accounts_json=$directoryraw AND (i.request_hash IS NULL OR i.request_hash=$hash) FOREACH (_ IN CASE WHEN i.request_hash IS NULL THEN [1] ELSE [] END | SET i.snapshot=$snapshot,i.request_hash=$hash,i.tenant_id=e.tenant_id,i.school_id=e.school_id,i.student_id=s.studentId,i.enrolled_student_id=e.student_id,i.payer_user_id=u.id,i.status='pending',i.version=0,i.issued_by=actor.id,i.issued_at=datetime() CREATE(:SchoolInvoiceAudit {{id:$audit,invoice_id:i.id,actor_id:actor.id,action:'issued',version:0,created_at:datetime(),eligibility_contract:'sis-current-family-v1',payer_user_id:u.id,student_id:s.studentId,enrolled_student_id:e.student_id,school_id:e.school_id,tenant_id:e.tenant_id,request_hash:$hash}})) {RETURN}",auth::fresh("i,config,ps"),carry(FAMILY,"i,config,ps,actor"),auth::scope("e.school_id","e.tenant_id"));
    let q = staff_query(query(&body), a, false)
        .param("payer", snapshot.payer_user_id.clone())
        .param("student", snapshot.student_id.clone())
        .param("school", snapshot.school_id.clone())
        .param("tenant", snapshot.tenant_id.clone())
        .param("enrolled", snapshot.enrolled_student_id.clone())
        .param("bankraw", bankraw)
        .param("directoryraw", directoryraw)
        .param("payee_version", payee_version)
        .param("key", issuekey)
        .param("id", snapshot.id)
        .param("snapshot", raw)
        .param("hash", hash)
        .param("audit", uuid::Uuid::new_v4().to_string());
    write(graph, q, a.expires, invoice).await
}
fn carry(q: &str, vars: &str) -> String {
    q.replace("WITH DISTINCT u,s", &format!("WITH DISTINCT {vars},u,s"))
        .replace("WITH u,s,collect", &format!("WITH {vars},u,s,collect"))
        .replace("WITH u,s,head", &format!("WITH {vars},u,s,head"))
}
fn carry_actor(q: &str) -> String {
    carry(q, "actor")
}

pub async fn list_parent(graph: &Graph, a: &auth::Parent) -> Result<Vec<Invoice>> {
    let q=query(&format!("MATCH (i:SchoolInvoice {{payer_user_id:$subject}}) {} {RETURN} ORDER BY i.issued_at DESC,i.id LIMIT 201",parent_guard())).param("subject",a.subject.clone()).param("expires",a.expires);
    valid_time(a.expires)?;
    let result = collect(graph, q).await?;
    valid_time(a.expires)?;
    Ok(result)
}
pub(super) async fn staff_authority(graph: &Graph, a: &auth::Staff) -> Result<()> {
    valid_time(a.expires)?;
    let mut principal=graph.execute(staff_query(query(&format!("{} AND (('owner' IN coalesce(actor.roles,[]) AND size(coalesce(actor.schoolIds,[]))=0 AND size(coalesce(actor.tenantIds,[]))=0) OR (size(coalesce(actor.schoolIds,[]))>0 AND size(coalesce(actor.tenantIds,[]))>0)) RETURN actor.id AS id",auth::STAFF)),a,true)).await.map_err(unavailable)?;
    if principal.next().await.map_err(unavailable)?.is_none() {
        return Err(Error::Denied);
    }
    valid_time(a.expires)?;
    Ok(())
}
pub async fn list_finance(graph: &Graph, a: &auth::Staff) -> Result<Vec<Invoice>> {
    staff_authority(graph, a).await?;
    let result = collect(
        graph,
        staff_query(
            query(&format!(
                "{} {RETURN} ORDER BY i.issued_at DESC,i.id LIMIT 201",
                finance_prefix()
            )),
            a,
            true,
        ),
    )
    .await?;
    valid_time(a.expires)?;
    Ok(result)
}
async fn collect(graph: &Graph, q: Query) -> Result<Vec<Invoice>> {
    let mut rows = graph.execute(q).await.map_err(unavailable)?;
    let mut invoices = vec![];
    while let Some(row) = rows.next().await.map_err(unavailable)? {
        if invoices.len() == 200 {
            return Err(Error::Unavailable);
        }
        invoices.push(invoice(&row)?);
    }
    Ok(invoices)
}
pub async fn parent(graph: &Graph, a: &auth::Parent, id: &str) -> Result<Invoice> {
    valid_time(a.expires)?;
    let found = collect(
        graph,
        query(&format!(
            "MATCH(i:SchoolInvoice {{id:$id,payer_user_id:$subject}}) {} {RETURN} LIMIT 2",
            parent_guard()
        ))
        .param("subject", a.subject.clone())
        .param("expires", a.expires)
        .param("id", id.to_string()),
    )
    .await?;
    valid_time(a.expires)?;
    if found.len() != 1 {
        return Err(Error::Denied);
    }
    found.into_iter().next().ok_or(Error::Denied)
}
pub async fn finance(graph: &Graph, a: &auth::Staff, id: &str) -> Result<Invoice> {
    valid_time(a.expires)?;
    let found = collect(
        graph,
        staff_query(
            query(&format!(
                "{} AND i.id=$id {RETURN} LIMIT 2",
                finance_prefix()
            )),
            a,
            true,
        )
        .param("id", id.to_string()),
    )
    .await?;
    valid_time(a.expires)?;
    if found.len() != 1 {
        return Err(Error::Denied);
    }
    found.into_iter().next().ok_or(Error::Denied)
}
pub async fn proofs(graph: &Graph, id: &str) -> Result<Vec<Proof>> {
    let mut rows=graph.execute(query("MATCH(i:SchoolInvoice {id:$id})-[:HAS_PROOF]->(p:SchoolInvoiceProof) RETURN p.payload AS payload ORDER BY p.uploaded_at DESC LIMIT 101").param("id",id.to_string())).await.map_err(unavailable)?;
    let mut found = vec![];
    while let Some(row) = rows.next().await.map_err(unavailable)? {
        if found.len() == 100 {
            return Err(Error::Unavailable);
        }
        found.push(
            serde_json::from_str(&row.get::<String>("payload").ok_or(Error::Unavailable)?)
                .map_err(unavailable)?,
        );
    }
    Ok(found)
}
pub async fn record_proof(
    graph: &Graph,
    a: &auth::Parent,
    expected: &Invoice,
    p: &Proof,
    key: &str,
) -> Result<Invoice> {
    let body=format!("MATCH(i:SchoolInvoice {{id:$id,payer_user_id:$subject}}) SET i._lock=coalesce(i._lock,0)+1 WITH i {} AND i.version=$version AND i.status IN ['pending','rejected'] AND NOT EXISTS {{MATCH(i)-[:HAS_PROOF]->(:SchoolInvoiceProof {{document_hash:$hash,status:'submitted'}})}} WITH DISTINCT i,u CREATE(p:SchoolInvoiceProof {{id:$proof,key:$proofkey,invoice_id:i.id,document_hash:$hash,payload:$payload,object_key:$object,status:'submitted',uploaded_by:u.id,uploaded_at:datetime()}}) CREATE(i)-[:HAS_PROOF]->(p) SET i.status='pending_verification',i.version=i.version+1 CREATE(:SchoolInvoiceAudit {{id:$audit,invoice_id:i.id,actor_id:u.id,action:'proof_submitted',proof_id:p.id,version:i.version,created_at:datetime()}}) {RETURN}",parent_guard());
    let q = query(&body)
        .param("id", expected.snapshot.id.clone())
        .param("subject", a.subject.clone())
        .param("expires", a.expires)
        .param("version", expected.version)
        .param("hash", p.document_hash.clone())
        .param("proof", p.id.clone())
        .param(
            "proofkey",
            format!(
                "{}|{}|{}",
                expected.snapshot.id, expected.version, p.document_hash
            ),
        )
        .param("payload", serde_json::to_string(p).map_err(unavailable)?)
        .param("object", key.to_string())
        .param("audit", uuid::Uuid::new_v4().to_string());
    write(graph, q, a.expires, invoice).await
}
pub async fn review(graph: &Graph, a: &auth::Staff, id: &str, input: &Review) -> Result<Invoice> {
    let status = if input.decision == "approve" {
        "paid"
    } else {
        "rejected"
    };
    let body=format!("MATCH(i:SchoolInvoice {{id:$id}}) SET i._lock=coalesce(i._lock,0)+1 WITH i MATCH(i)-[:HAS_PROOF]->(p:SchoolInvoiceProof {{id:$proof}}) SET p._lock=coalesce(p._lock,0)+1 WITH i,p {} WITH actor,i,p WHERE {} AND i.version=$version AND i.status='pending_verification' AND p.status='submitted' SET p.status=$status,p.reviewed_by=actor.id,p.review_note=$note,p.reviewed_at=datetime(),i.status=$status,i.version=i.version+1 FOREACH(_ IN CASE WHEN $status='paid' THEN [1] ELSE [] END | CREATE(:SchoolInvoiceSettlement {{invoice_id:i.id,id:$settlement,proof_id:p.id,amount:$amount,currency:'IDR',verified_by:actor.id,created_at:datetime()}}) SET i.paid_at=toString(datetime()),i.receipt_ref=$receipt) CREATE(:SchoolInvoiceAudit {{id:$audit,invoice_id:i.id,actor_id:actor.id,action:$action,proof_id:p.id,version:i.version,note:$note,created_at:datetime()}}) {RETURN}",auth::fresh("i,p"),auth::scope("i.school_id","i.tenant_id"));
    let inv = finance(graph, a, id).await?;
    // Only full-value proofs settle this first contract. Partial/overpayment needs
    // an explicit reconciliation model; neither can silently pay the invoice.
    let ps = proofs(graph, id).await?;
    let p = ps
        .iter()
        .find(|p| p.id == input.proof_id)
        .ok_or(Error::Denied)?;
    if input.decision == "approve" && p.amount_submitted != inv.snapshot.amount {
        return Err(Error::Conflict);
    }
    let q = staff_query(query(&body), a, true)
        .param("id", id.to_string())
        .param("version", input.version)
        .param("proof", input.proof_id.clone())
        .param("status", status)
        .param("note", input.note.clone())
        .param("amount", inv.snapshot.amount)
        .param("settlement", format!("SSET-{}", uuid::Uuid::new_v4()))
        .param("receipt", format!("SRCPT-{}", uuid::Uuid::new_v4()))
        .param("audit", uuid::Uuid::new_v4().to_string())
        .param(
            "action",
            if status == "paid" {
                "settled"
            } else {
                "proof_rejected"
            },
        );
    write(graph, q, a.expires, invoice).await
}
pub async fn proof_object(graph: &Graph, invoice: &str, proof: &str) -> Result<(String, Proof)> {
    let mut rows=graph.execute(query("MATCH(:SchoolInvoice {id:$id})-[:HAS_PROOF]->(p:SchoolInvoiceProof {id:$proof}) RETURN p.object_key AS object,p.payload AS payload LIMIT 2").param("id",invoice.to_string()).param("proof",proof.to_string())).await.map_err(unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(unavailable)?
        .ok_or(Error::Denied)?;
    if rows.next().await.map_err(unavailable)?.is_some() {
        return Err(Error::Denied);
    }
    Ok((
        row.get("object").ok_or(Error::Unavailable)?,
        serde_json::from_str(&row.get::<String>("payload").ok_or(Error::Unavailable)?)
            .map_err(unavailable)?,
    ))
}
