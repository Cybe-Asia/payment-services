//! Graph I/O for school billing (ADR 0014). Payment-owned labels only; the
//! child/payer relation is always re-read through `sis-current-family-v1`.
use super::model::*;
use crate::school_invoice::{
    auth,
    model::{identifier, Snapshot},
    repository::{self as invoices, Error, Result},
};
use neo4rs::{query, Graph, Query, Row};
use serde::Serialize;

const FAMILY: &str = include_str!("../school_invoice/contracts/sis-current-family-v1.cypher");

fn unavailable(_: impl std::fmt::Debug) -> Error {
    Error::Unavailable
}

pub async fn init(graph: &Graph) -> Result<()> {
    for (name, label, prop) in [
        ("school_fee_plan_key", "SchoolFeePlan", "key"),
        ("monthly_fee_selection_key", "MonthlyFeeSelection", "key"),
        ("capital_levy_id", "CapitalLevyObligation", "id"),
        ("capital_levy_key", "CapitalLevyObligation", "key"),
        ("capital_levy_plan_key", "CapitalLevyPlan", "key"),
        ("overseas_program_id", "OverseasProgram", "id"),
        ("overseas_enrollment_id", "OverseasEnrollment", "id"),
        ("overseas_enrollment_key", "OverseasEnrollment", "key"),
        (
            "school_billing_line_invoice",
            "SchoolBillingLine",
            "invoice_id",
        ),
        (
            "school_invoice_checkout_number",
            "SchoolInvoiceCheckout",
            "invoice_number",
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

async fn rows(graph: &Graph, q: Query, max: usize) -> Result<Vec<Row>> {
    let mut stream = graph.execute(q).await.map_err(unavailable)?;
    let mut found = vec![];
    while let Some(row) = stream.next().await.map_err(unavailable)? {
        if found.len() == max {
            return Err(Error::Unavailable);
        }
        found.push(row);
    }
    Ok(found)
}
async fn one(graph: &Graph, q: Query) -> Result<Row> {
    let mut found = rows(graph, q, 2).await.map_err(|_| Error::Denied)?;
    if found.len() != 1 {
        return Err(Error::Denied);
    }
    Ok(found.remove(0))
}
fn s(row: &Row, k: &str) -> Result<String> {
    row.get::<String>(k).ok_or(Error::Unavailable)
}
fn n(row: &Row, k: &str) -> Result<i64> {
    row.get::<i64>(k).ok_or(Error::Unavailable)
}
/// A value that may be negative, returned by the query as `toString(...)`:
/// neo4rs 0.6 decodes the tiny integers -16..-1 as 240..255, so -1 would
/// read as 255.
fn signed(row: &Row, k: &str) -> Result<i64> {
    s(row, k)?.parse().map_err(unavailable)
}

/// A child the payer may be billed for right now.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Child {
    pub student_id: String,
    pub enrolled_student_id: String,
    pub name: String,
    pub tenant_id: String,
    pub school_id: String,
    pub school_name: String,
    pub year_group: String,
    #[serde(skip)]
    pub payer_user_id: String,
}
const CHILD: &str = "RETURN u.id AS payer,s.studentId AS student,e.student_id AS enrolled,coalesce(s.fullName,'') AS name,e.tenant_id AS tenant,e.school_id AS school,coalesce(school.name,school.school_name,school.school_code,'') AS school_name,coalesce(toString(sec.year_group),'') AS year LIMIT 2";
fn child(row: &Row) -> Result<Child> {
    Ok(Child {
        payer_user_id: s(row, "payer")?,
        student_id: s(row, "student")?,
        enrolled_student_id: s(row, "enrolled")?,
        name: s(row, "name")?,
        tenant_id: s(row, "tenant")?,
        school_id: s(row, "school")?,
        school_name: s(row, "school_name")?,
        year_group: s(row, "year")?,
    })
}
/// The current-family read for one payer and child; `Denied` if it fails.
pub async fn family(graph: &Graph, payer: &str, student: &str) -> Result<Child> {
    if !identifier(payer) || !identifier(student) {
        return Err(Error::Denied);
    }
    child(
        &one(
            graph,
            query(&format!("{FAMILY} {CHILD}"))
                .param("payer", payer.to_string())
                .param("student", student.to_string()),
        )
        .await?,
    )
}
/// Every child of a signed-in parent that passes the family read.
pub async fn children(graph: &Graph, a: &auth::Parent) -> Result<Vec<Child>> {
    let ids = rows(
        graph,
        query("MATCH (:User {id:$payer})-[:HAS_APPLICATION]->(:Lead)-[:HAS_STUDENT]->(s:Student) WHERE coalesce(s.studentId,'')<>'' RETURN DISTINCT s.studentId AS id ORDER BY id LIMIT 21")
            .param("payer", a.subject.clone()),
        20,
    )
    .await?;
    let mut found = vec![];
    for row in ids {
        match family(graph, &a.subject, &s(&row, "id")?).await {
            Ok(c) => found.push(c),
            Err(Error::Denied) => {}
            Err(e) => return Err(e),
        }
    }
    if !auth::live(a.expires) {
        return Err(Error::Expired);
    }
    Ok(found)
}

// ---------------------------------------------------------------- invoices

/// One billed invoice as the Payment tab shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bill {
    pub invoice_id: String,
    pub category: String,
    pub kind: String,
    pub ref_id: String,
    pub status: String,
    pub amount: i64,
    pub gross: i64,
    pub discount: i64,
    pub due_date: String,
    pub paid_at: Option<String>,
    pub period: String,
    pub period_start: String,
    pub months: i64,
    pub installment: i64,
    pub issued_at: String,
}
impl Bill {
    pub fn open(&self) -> bool {
        matches!(
            self.status.as_str(),
            "pending" | "pending_verification" | "rejected"
        )
    }
    pub fn paid(&self) -> bool {
        self.status == "paid"
    }
}
/// Billing lines of one child for one payer, newest first (what that payer sees).
pub async fn bills(graph: &Graph, payer: &str, enrolled: &str) -> Result<Vec<Bill>> {
    bills_where(graph, "i.payer_user_id=$payer", payer, enrolled).await
}
/// Every billing line of one child, whoever pays: what is already billed for
/// the child, so a second guardian is never billed the same months again.
pub async fn child_bills(graph: &Graph, enrolled: &str) -> Result<Vec<Bill>> {
    bills_where(graph, "true", "", enrolled).await
}
async fn bills_where(graph: &Graph, scope: &str, payer: &str, enrolled: &str) -> Result<Vec<Bill>> {
    let found = rows(
        graph,
        query(&format!("MATCH (i:SchoolInvoice {{enrolled_student_id:$enrolled}})-[:BILLED_AS]->(l:SchoolBillingLine) WHERE l.invoice_id=i.id AND {scope} RETURN i.id AS id,i.status AS status,i.paid_at AS paid,i.snapshot AS snapshot,l.category AS category,l.kind AS kind,l.ref_id AS ref,l.period_start AS period_start,l.months AS months,toString(l.installment) AS installment,l.gross AS gross,l.discount AS discount,l.amount AS amount ORDER BY i.issued_at DESC,i.id LIMIT 501"))
            .param("payer", payer.to_string())
            .param("enrolled", enrolled.to_string()),
        500,
    )
    .await?;
    found.iter().map(bill).collect()
}
fn bill(row: &Row) -> Result<Bill> {
    let snapshot: Snapshot = serde_json::from_str(&s(row, "snapshot")?).map_err(unavailable)?;
    let amount = n(row, "amount")?;
    if snapshot.id != s(row, "id")? || snapshot.amount != amount {
        return Err(Error::Unavailable);
    }
    Ok(Bill {
        invoice_id: snapshot.id,
        category: s(row, "category")?,
        kind: s(row, "kind")?,
        ref_id: s(row, "ref")?,
        status: s(row, "status")?,
        amount,
        gross: n(row, "gross")?,
        discount: n(row, "discount")?,
        due_date: snapshot.due_date,
        paid_at: row.get("paid"),
        period: snapshot.period,
        period_start: s(row, "period_start")?,
        months: n(row, "months")?,
        installment: signed(row, "installment")?,
        issued_at: snapshot.issued_at,
    })
}

/// The invoice an earlier request with the same idempotency key issued.
pub async fn issued_by_key(graph: &Graph, c: &Child, key: &str) -> Result<Option<String>> {
    let found = rows(
        graph,
        query("MATCH (i:SchoolInvoice {issue_key:$key,payer_user_id:$payer}) RETURN i.id AS id LIMIT 2")
            .param("key", format!("{}|{}|{key}", c.tenant_id, c.school_id))
            .param("payer", c.payer_user_id.clone()),
        1,
    )
    .await?;
    found.first().map(|r| s(r, "id")).transpose()
}

// ---------------------------------------------------------------- fee plans

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeePlan {
    #[serde(flatten)]
    pub input: FeePlanInput,
    pub updated_at: Option<String>,
}
const PLAN: &str = "RETURN p.tenant_id AS tenant,p.school_id AS school,p.year_group AS year,p.monthly_amount AS monthly,p.options_json AS options,p.due_day AS due_day,p.issue_lead_days AS lead,p.active AS active,p.version AS version,toString(p.updated_at) AS updated";
fn plan(row: &Row) -> Result<FeePlan> {
    let input = FeePlanInput {
        tenant_id: s(row, "tenant")?,
        school_id: s(row, "school")?,
        year_group: s(row, "year")?,
        monthly_amount: n(row, "monthly")?,
        options: serde_json::from_str(&s(row, "options")?).map_err(unavailable)?,
        due_day: u32::try_from(n(row, "due_day")?).map_err(unavailable)?,
        issue_lead_days: n(row, "lead")?,
        active: row.get("active").ok_or(Error::Unavailable)?,
        version: n(row, "version")?,
    };
    if !input.valid() {
        return Err(Error::Unavailable);
    }
    Ok(FeePlan {
        input,
        updated_at: row.get("updated"),
    })
}
/// The active plan for a child: its year group's plan, else the school's `*`.
pub async fn plan_for(graph: &Graph, c: &Child) -> Result<Option<FeePlan>> {
    let found = rows(
        graph,
        query(&format!("MATCH (p:SchoolFeePlan {{tenant_id:$tenant,school_id:$school,active:true}}) WHERE p.year_group IN [$year,'*'] {PLAN} ORDER BY CASE p.year_group WHEN '*' THEN 1 ELSE 0 END LIMIT 2"))
            .param("tenant", c.tenant_id.clone())
            .param("school", c.school_id.clone())
            .param("year", c.year_group.clone()),
        2,
    )
    .await?;
    found.first().map(plan).transpose()
}

fn staff(q: Query, a: &auth::Staff) -> Query {
    q.param("subject", a.subject.clone())
        .param("staff", a.id.clone())
        .param("roles", auth::roles(false))
        .param("expires", a.expires)
}
/// Plans of every school the staff member may bill.
pub async fn plans(graph: &Graph, a: &auth::Staff) -> Result<Vec<FeePlan>> {
    let found = rows(
        graph,
        staff(
            query(&format!(
                "{} WITH actor MATCH (p:SchoolFeePlan) WHERE {} {PLAN} ORDER BY p.school_id,p.year_group LIMIT 501",
                auth::STAFF,
                auth::scope("p.school_id", "p.tenant_id")
            )),
            a,
        ),
        500,
    )
    .await?;
    found.iter().map(plan).collect()
}
pub async fn save_plan(graph: &Graph, a: &auth::Staff, p: &FeePlanInput) -> Result<FeePlan> {
    let body = format!(
        "MATCH (school:School {{school_id:$school,tenant_id:$tenant}}) MERGE (p:SchoolFeePlan {{key:$key}}) ON CREATE SET p.version=0 SET p._lock=coalesce(p._lock,0)+1 WITH p {} WITH actor,p WHERE {} AND p.version=$version SET p.tenant_id=$tenant,p.school_id=$school,p.year_group=$year,p.monthly_amount=$monthly,p.options_json=$options,p.due_day=$due_day,p.issue_lead_days=$lead,p.active=$active,p.version=p.version+1,p.updated_by=actor.id,p.updated_at=datetime() CREATE (:SchoolBillingAudit {{id:$audit,subject:'fee_plan',ref_id:p.key,actor_id:actor.id,action:'saved',version:p.version,payload:$payload,created_at:datetime()}}) {PLAN}",
        auth::fresh("p"),
        auth::scope("$school", "$tenant")
    );
    let q = staff(query(&body), a)
        .param(
            "key",
            format!("{}|{}|{}", p.tenant_id, p.school_id, p.year_group),
        )
        .param("tenant", p.tenant_id.clone())
        .param("school", p.school_id.clone())
        .param("year", p.year_group.clone())
        .param("monthly", p.monthly_amount)
        .param(
            "options",
            serde_json::to_string(&p.options).map_err(unavailable)?,
        )
        .param("due_day", i64::from(p.due_day))
        .param("lead", p.issue_lead_days)
        .param("active", p.active)
        .param("version", p.version)
        .param("payload", serde_json::to_string(p).map_err(unavailable)?)
        .param("audit", uuid::Uuid::new_v4().to_string());
    invoices::write(graph, q, a.expires, plan).await
}

// ---------------------------------------------------------------- selection

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub months: i32,
    pub version: i64,
}
pub async fn selection(graph: &Graph, c: &Child) -> Result<Option<Selection>> {
    let found = rows(
        graph,
        query("MATCH (m:MonthlyFeeSelection {key:$key}) RETURN m.months AS months,m.version AS version LIMIT 2")
            .param("key", selection_key(c)),
        1,
    )
    .await?;
    found
        .first()
        .map(|r| {
            Ok(Selection {
                months: i32::try_from(n(r, "months")?).map_err(unavailable)?,
                version: n(r, "version")?,
            })
        })
        .transpose()
}
fn selection_key(c: &Child) -> String {
    format!("{}|{}|{}", c.tenant_id, c.school_id, c.enrolled_student_id)
}
/// Records the parent's period. Refused while any monthly-fee invoice of the
/// child is open, so a change never overlaps a bill already sent.
pub async fn select(
    graph: &Graph,
    a: &auth::Parent,
    c: &Child,
    months: i32,
    version: i64,
) -> Result<Selection> {
    let body = "MERGE (m:MonthlyFeeSelection {key:$key}) ON CREATE SET m.version=0 SET m._lock=coalesce(m._lock,0)+1 WITH m WHERE m.version=$version AND coalesce(m.payer_user_id,$payer)=$payer AND NOT EXISTS { MATCH (i:SchoolInvoice {enrolled_student_id:$enrolled})-[:BILLED_AS]->(:SchoolBillingLine {category:'monthly_fee'}) WHERE i.status IN ['pending','pending_verification','rejected'] } AND datetime.realtime().epochSeconds < $expires SET m.tenant_id=$tenant,m.school_id=$school,m.student_id=$student,m.enrolled_student_id=$enrolled,m.payer_user_id=$payer,m.months=$months,m.version=m.version+1,m.updated_at=datetime() CREATE (:SchoolBillingAudit {id:$audit,subject:'monthly_fee_selection',ref_id:m.key,actor_id:$payer,action:'selected',version:m.version,payload:toString($months),created_at:datetime()}) RETURN m.months AS months,m.version AS version";
    let q = query(body)
        .param("key", selection_key(c))
        .param("version", version)
        .param("expires", a.expires)
        .param("tenant", c.tenant_id.clone())
        .param("school", c.school_id.clone())
        .param("student", c.student_id.clone())
        .param("enrolled", c.enrolled_student_id.clone())
        .param("payer", a.subject.clone())
        .param("months", i64::from(months))
        .param("audit", uuid::Uuid::new_v4().to_string());
    invoices::write(graph, q, a.expires, |r| {
        Ok(Selection {
            months: i32::try_from(n(r, "months")?).map_err(unavailable)?,
            version: n(r, "version")?,
        })
    })
    .await
}
/// Children with a chosen period, for the issuing worker.
pub async fn selections(graph: &Graph) -> Result<Vec<(String, String)>> {
    rows(
        graph,
        query("MATCH (m:MonthlyFeeSelection) RETURN m.payer_user_id AS payer,m.student_id AS student ORDER BY m.key LIMIT 20001"),
        20_000,
    )
    .await?
    .iter()
    .map(|r| Ok((s(r, "payer")?, s(r, "student")?)))
    .collect()
}

// ---------------------------------------------------------------- schedules

/// A Capital Levy obligation or an Overseas enrollment: a payable total with
/// an installment schedule, billed for one child.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub category: String,
    pub student_id: String,
    pub enrolled_student_id: String,
    #[serde(skip)]
    pub payer_user_id: String,
    pub student_name: String,
    pub gross: i64,
    pub discount: i64,
    pub payable: i64,
    pub installments: Vec<Installment>,
    pub issue_lead_days: i64,
    pub scheme: String,
    pub program_id: String,
    /// Capital Levy only: start year of the academic year it is owed for.
    pub academic_year: String,
    pub version: i64,
    pub created_at: String,
}
const SCHEDULE: &str = "RETURN o.id AS id,o.category AS category,o.student_id AS student,o.enrolled_student_id AS enrolled,o.payer_user_id AS payer,coalesce(o.student_name,'') AS name,o.gross AS gross,o.discount AS discount,o.payable AS payable,o.installments_json AS installments,o.issue_lead_days AS lead,coalesce(o.scheme,'') AS scheme,coalesce(o.program_id,'') AS program,coalesce(o.academic_year,'') AS ay,o.version AS version,toString(o.created_at) AS created";
fn schedule(row: &Row) -> Result<Schedule> {
    let installments: Vec<Installment> =
        serde_json::from_str(&s(row, "installments")?).map_err(unavailable)?;
    if !valid_schedule(&installments) {
        return Err(Error::Unavailable);
    }
    Ok(Schedule {
        id: s(row, "id")?,
        category: s(row, "category")?,
        student_id: s(row, "student")?,
        enrolled_student_id: s(row, "enrolled")?,
        payer_user_id: s(row, "payer")?,
        student_name: s(row, "name")?,
        gross: n(row, "gross")?,
        discount: n(row, "discount")?,
        payable: n(row, "payable")?,
        installments,
        issue_lead_days: n(row, "lead")?,
        scheme: s(row, "scheme")?,
        program_id: s(row, "program")?,
        academic_year: s(row, "ay")?,
        version: n(row, "version")?,
        created_at: s(row, "created")?,
    })
}
/// The child's Capital Levy of every academic year billed through the
/// Payment tab, newest year first.
pub async fn capital_levies_for(graph: &Graph, c: &Child) -> Result<Vec<Schedule>> {
    rows(
        graph,
        query(&format!("MATCH (o:CapitalLevyObligation {{enrolled_student_id:$enrolled,tenant_id:$tenant,school_id:$school}}) WHERE o.payer_user_id=$payer {SCHEDULE} ORDER BY o.academic_year DESC LIMIT 21"))
            .param("enrolled", c.enrolled_student_id.clone())
            .param("tenant", c.tenant_id.clone())
            .param("school", c.school_id.clone())
            .param("payer", c.payer_user_id.clone()),
        20,
    )
    .await?
    .iter()
    .map(schedule)
    .collect()
}
/// What the child's accepted, paid admissions offer covered.
pub async fn admission_paid(graph: &Graph, c: &Child) -> Result<Option<AdmissionPaid>> {
    let found = rows(
        graph,
        query("MATCH (:Student {studentId:$student})-[:HAS_OFFER]->(o:Offer {status:'accepted'})-[:PAID_VIA]->(p:Payment {status:'paid'}) WHERE p.payment_type IN ['offer_due_now','enrolment_fee'] AND coalesce(o.tenant_id,$tenant)=$tenant AND coalesce(o.target_school_id,$school)=$school AND o.pricing_snapshot_json IS NOT NULL RETURN toString(o.academic_year) AS ay,o.pricing_snapshot_json AS pricing ORDER BY o.issued_at DESC LIMIT 6")
            .param("student", c.student_id.clone())
            .param("tenant", c.tenant_id.clone())
            .param("school", c.school_id.clone()),
        5,
    )
    .await?;
    Ok(found
        .iter()
        .find_map(|r| admission_paid_from(&r.get::<String>("ay")?, &r.get::<String>("pricing")?)))
}
use super::model::admission_paid as admission_paid_from;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevyPlan {
    #[serde(flatten)]
    pub input: CapitalLevyPlanInput,
    pub updated_at: Option<String>,
}
const LEVY_PLAN: &str =
    "RETURN p.payload AS payload,p.version AS version,toString(p.updated_at) AS updated";
fn levy_plan(row: &Row) -> Result<LevyPlan> {
    let mut input: CapitalLevyPlanInput =
        serde_json::from_str(&s(row, "payload")?).map_err(unavailable)?;
    input.version = n(row, "version")?;
    if !input.valid() {
        return Err(Error::Unavailable);
    }
    Ok(LevyPlan {
        input,
        updated_at: row.get("updated"),
    })
}
pub async fn levy_plans(graph: &Graph, a: &auth::Staff) -> Result<Vec<LevyPlan>> {
    rows(
        graph,
        staff(
            query(&format!(
                "{} WITH actor MATCH (p:CapitalLevyPlan) WHERE {} {LEVY_PLAN} ORDER BY p.academic_year DESC,p.school_id,p.year_group LIMIT 501",
                auth::STAFF,
                auth::scope("p.school_id", "p.tenant_id")
            )),
            a,
        ),
        500,
    )
    .await?
    .iter()
    .map(levy_plan)
    .collect()
}
pub async fn save_levy_plan(
    graph: &Graph,
    a: &auth::Staff,
    p: &CapitalLevyPlanInput,
) -> Result<LevyPlan> {
    let body = format!(
        "MATCH (school:School {{school_id:$school,tenant_id:$tenant}}) MERGE (p:CapitalLevyPlan {{key:$key}}) ON CREATE SET p.version=0 SET p._lock=coalesce(p._lock,0)+1 WITH p {} WITH actor,p WHERE {} AND p.version=$version SET p.tenant_id=$tenant,p.school_id=$school,p.year_group=$year,p.academic_year=$ay,p.active=$active,p.payload=$payload,p.version=p.version+1,p.updated_by=actor.id,p.updated_at=datetime() CREATE (:SchoolBillingAudit {{id:$audit,subject:'capital_levy_plan',ref_id:p.key,actor_id:actor.id,action:'saved',version:p.version,payload:$payload,created_at:datetime()}}) {LEVY_PLAN}",
        auth::fresh("p"),
        auth::scope("$school", "$tenant")
    );
    let q = staff(query(&body), a)
        .param(
            "key",
            format!(
                "{}|{}|{}|{}",
                p.tenant_id, p.school_id, p.year_group, p.academic_year
            ),
        )
        .param("tenant", p.tenant_id.clone())
        .param("school", p.school_id.clone())
        .param("year", p.year_group.clone())
        .param("ay", p.academic_year.clone())
        .param("active", p.active)
        .param("version", p.version)
        .param("payload", serde_json::to_string(p).map_err(unavailable)?)
        .param("audit", uuid::Uuid::new_v4().to_string());
    invoices::write(graph, q, a.expires, levy_plan).await
}
/// Active plans that apply to a child: per academic year, its year group's
/// plan wins over the school's `*` plan.
pub async fn levy_plans_for(graph: &Graph, c: &Child) -> Result<Vec<LevyPlan>> {
    let found: Vec<LevyPlan> = rows(
        graph,
        query(&format!("MATCH (p:CapitalLevyPlan {{tenant_id:$tenant,school_id:$school,active:true}}) WHERE p.year_group IN [$year,'*'] {LEVY_PLAN} ORDER BY p.academic_year,CASE p.year_group WHEN '*' THEN 1 ELSE 0 END LIMIT 41"))
            .param("tenant", c.tenant_id.clone())
            .param("school", c.school_id.clone())
            .param("year", c.year_group.clone()),
        40,
    )
    .await?
    .iter()
    .map(levy_plan)
    .collect::<Result<_>>()?;
    let mut seen = std::collections::BTreeSet::new();
    Ok(found
        .into_iter()
        .filter(|p| seen.insert(p.input.academic_year.clone()))
        .collect())
}
/// Creates the child's obligation for a plan's year, or brings it in line with
/// the plan while nothing has been billed against it yet.
pub async fn ensure_levy(graph: &Graph, c: &Child, p: &CapitalLevyPlanInput) -> Result<()> {
    let total = schedule_total(&p.installments);
    let q = query("MERGE (o:CapitalLevyObligation {key:$key}) ON CREATE SET o.id=$id,o.version=0,o.created_at=datetime(),o.payer_user_id=$payer SET o._lock=coalesce(o._lock,0)+1 WITH o WHERE NOT EXISTS { MATCH (:SchoolBillingLine {category:'capital_levy',ref_id:o.id}) } AND (o.plan_payload IS NULL OR o.plan_payload<>$payload) SET o.category='capital_levy',o.tenant_id=$tenant,o.school_id=$school,o.student_id=$student,o.enrolled_student_id=$enrolled,o.student_name=$name,o.academic_year=$ay,o.gross=$total,o.discount=0,o.payable=$total,o.installments_json=$installments,o.issue_lead_days=$lead,o.plan_payload=$payload,o.version=o.version+1,o.updated_at=datetime() CREATE (:SchoolBillingAudit {id:$audit,subject:'capital_levy',ref_id:o.id,actor_id:'system:school-billing',action:'from_plan',version:o.version,payload:$ay,created_at:datetime()}) RETURN o.id AS id")
        .param("key", format!("{}|{}|{}|{}", c.tenant_id, c.school_id, c.enrolled_student_id, p.academic_year))
        .param("id", format!("CLEVY-{}", uuid::Uuid::new_v4()))
        .param("payer", c.payer_user_id.clone())
        .param("tenant", c.tenant_id.clone())
        .param("school", c.school_id.clone())
        .param("student", c.student_id.clone())
        .param("enrolled", c.enrolled_student_id.clone())
        .param("name", c.name.clone())
        .param("ay", p.academic_year.clone())
        .param("total", total)
        .param("installments", serde_json::to_string(&p.installments).map_err(unavailable)?)
        .param("lead", p.issue_lead_days)
        .param("payload", serde_json::to_string(p).map_err(unavailable)?)
        .param("audit", uuid::Uuid::new_v4().to_string());
    graph.run(q).await.map_err(unavailable)
}
/// Payer and child of every active enrolment of a school, for creating the
/// Capital Levy of a plan's year.
pub async fn school_families(
    graph: &Graph,
    tenant: &str,
    school: &str,
) -> Result<Vec<(String, String)>> {
    rows(
        graph,
        query("MATCH (u:User)-[:HAS_APPLICATION]->(:Lead)-[:HAS_STUDENT]->(s:Student)-[:ENROLLED_AS]->(e:EnrolledStudent {status:'active',tenant_id:$tenant,school_id:$school}) RETURN DISTINCT u.id AS payer,s.studentId AS student ORDER BY student,payer LIMIT 5001")
            .param("tenant", tenant.to_string())
            .param("school", school.to_string()),
        5000,
    )
    .await?
    .iter()
    .map(|r| Ok((s(r, "payer")?, s(r, "student")?)))
    .collect()
}
/// Schools with an active Capital Levy plan, for the worker.
pub async fn levy_plan_schools(graph: &Graph) -> Result<Vec<(String, String)>> {
    rows(
        graph,
        query("MATCH (p:CapitalLevyPlan {active:true}) RETURN DISTINCT p.tenant_id AS tenant,p.school_id AS school LIMIT 201"),
        200,
    )
    .await?
    .iter()
    .map(|r| Ok((s(r, "tenant")?, s(r, "school")?)))
    .collect()
}
pub async fn enrollments(graph: &Graph, c: &Child) -> Result<Vec<Schedule>> {
    rows(
        graph,
        query(&format!("MATCH (o:OverseasEnrollment {{enrolled_student_id:$enrolled,payer_user_id:$payer}}) {SCHEDULE} ORDER BY o.created_at LIMIT 21"))
            .param("enrolled", c.enrolled_student_id.clone())
            .param("payer", c.payer_user_id.clone()),
        20,
    )
    .await?
    .iter()
    .map(schedule)
    .collect()
}
/// Every schedule, for the issuing worker.
pub async fn all_schedules(graph: &Graph) -> Result<Vec<Schedule>> {
    rows(
        graph,
        query(&format!("MATCH (o) WHERE o:CapitalLevyObligation OR o:OverseasEnrollment {SCHEDULE} ORDER BY o.id LIMIT 20001")),
        20_000,
    )
    .await?
    .iter()
    .map(schedule)
    .collect()
}

/// An enrolled child in the staff member's scope, for Admin pickers.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaffStudent {
    pub student_id: String,
    pub enrolled_student_id: String,
    pub name: String,
    pub tenant_id: String,
    pub school_id: String,
    pub year_group: String,
    pub section: String,
}
pub async fn staff_students(graph: &Graph, a: &auth::Staff) -> Result<Vec<StaffStudent>> {
    let found = rows(
        graph,
        staff(
            query(&format!(
                "{} WITH actor MATCH (s:Student)-[:ENROLLED_AS]->(e:EnrolledStudent {{status:'active'}})-[:ENROLLED_IN]->(sec:Section {{status:'active'}}) WHERE {} AND e.applicant_student_id=s.studentId RETURN DISTINCT s.studentId AS student,e.student_id AS enrolled,coalesce(s.fullName,'') AS name,e.tenant_id AS tenant,e.school_id AS school,coalesce(toString(sec.year_group),'') AS year,coalesce(sec.name,'') AS section ORDER BY name,student LIMIT 2001",
                auth::STAFF,
                auth::scope("e.school_id", "e.tenant_id")
            )),
            a,
        ),
        2000,
    )
    .await?;
    found
        .iter()
        .map(|r| {
            Ok(StaffStudent {
                student_id: s(r, "student")?,
                enrolled_student_id: s(r, "enrolled")?,
                name: s(r, "name")?,
                tenant_id: s(r, "tenant")?,
                school_id: s(r, "school")?,
                year_group: s(r, "year")?,
                section: s(r, "section")?,
            })
        })
        .collect()
}

pub async fn capital_levies(graph: &Graph, a: &auth::Staff) -> Result<Vec<Schedule>> {
    rows(
        graph,
        staff(
            query(&format!(
                "{} WITH actor MATCH (o:CapitalLevyObligation) WHERE {} {SCHEDULE} ORDER BY o.created_at DESC LIMIT 2001",
                auth::STAFF,
                auth::scope("o.school_id", "o.tenant_id")
            )),
            a,
        ),
        2000,
    )
    .await?
    .iter()
    .map(schedule)
    .collect()
}
// ---------------------------------------------------------------- overseas

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    pub id: String,
    #[serde(flatten)]
    pub input: OverseasProgramInput,
    pub terms_version: String,
    pub participants: i64,
}
const PROGRAM: &str = "RETURN p.id AS id,p.payload AS payload,p.terms_version AS terms_version,p.version AS version,COUNT { (p)<-[:JOINED]-(:OverseasEnrollment) } AS participants";
fn program(row: &Row) -> Result<Program> {
    let mut input: OverseasProgramInput =
        serde_json::from_str(&s(row, "payload")?).map_err(unavailable)?;
    let terms_version = s(row, "terms_version")?;
    if !input.valid() || input.terms_version() != terms_version {
        return Err(Error::Unavailable);
    }
    input.version = n(row, "version")?;
    Ok(Program {
        id: s(row, "id")?,
        input,
        terms_version,
        participants: n(row, "participants")?,
    })
}
pub async fn programs(graph: &Graph, a: &auth::Staff) -> Result<Vec<Program>> {
    rows(
        graph,
        staff(
            query(&format!(
                "{} WITH actor MATCH (p:OverseasProgram) WHERE {} {PROGRAM} ORDER BY p.created_at DESC LIMIT 201",
                auth::STAFF,
                auth::scope("p.school_id", "p.tenant_id")
            )),
            a,
        ),
        200,
    )
    .await?
    .iter()
    .map(program)
    .collect()
}
/// Open (or joined) programs a child may see.
pub async fn programs_for(graph: &Graph, c: &Child) -> Result<Vec<Program>> {
    rows(
        graph,
        query(&format!("MATCH (p:OverseasProgram {{tenant_id:$tenant,school_id:$school}}) WHERE (p.status='open' AND (size(p.year_groups)=0 OR $year IN p.year_groups)) OR EXISTS {{ MATCH (p)<-[:JOINED]-(:OverseasEnrollment {{enrolled_student_id:$enrolled}}) }} {PROGRAM} ORDER BY p.created_at DESC LIMIT 21"))
            .param("tenant", c.tenant_id.clone())
            .param("school", c.school_id.clone())
            .param("year", c.year_group.clone())
            .param("enrolled", c.enrolled_student_id.clone()),
        20,
    )
    .await?
    .iter()
    .map(program)
    .collect()
}
pub async fn program_for(graph: &Graph, c: &Child, id: &str) -> Result<Program> {
    programs_for(graph, c)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or(Error::Denied)
}
/// Creates (`id` None, version 0) or updates a program. Installments and the
/// discount of an existing enrollment are its own snapshot and never change.
pub async fn save_program(
    graph: &Graph,
    a: &auth::Staff,
    id: Option<&str>,
    p: &OverseasProgramInput,
) -> Result<Program> {
    let id = match id {
        Some(v) if identifier(v) && v.starts_with("OVS-") => v.to_string(),
        Some(_) => return Err(Error::Denied),
        None if p.version == 0 => format!("OVS-{}", uuid::Uuid::new_v4()),
        None => return Err(Error::Conflict),
    };
    let body = format!(
        "MATCH (school:School {{school_id:$school,tenant_id:$tenant}}) MERGE (p:OverseasProgram {{id:$id}}) ON CREATE SET p.version=0,p.created_at=datetime(),p.tenant_id=$tenant,p.school_id=$school SET p._lock=coalesce(p._lock,0)+1 WITH p {} WITH actor,p WHERE {} AND p.version=$version AND p.tenant_id=$tenant AND p.school_id=$school SET p.payload=$payload,p.terms_version=$terms,p.status=$status,p.year_groups=$years,p.version=p.version+1,p.updated_by=actor.id,p.updated_at=datetime() CREATE (:SchoolBillingAudit {{id:$audit,subject:'overseas_program',ref_id:p.id,actor_id:actor.id,action:'saved',version:p.version,payload:$terms,created_at:datetime()}}) {PROGRAM}",
        auth::fresh("p"),
        auth::scope("$school", "$tenant")
    );
    let q = staff(query(&body), a)
        .param("id", id)
        .param("tenant", p.tenant_id.clone())
        .param("school", p.school_id.clone())
        .param("version", p.version)
        .param("payload", serde_json::to_string(p).map_err(unavailable)?)
        .param("terms", p.terms_version())
        .param("status", p.status.clone())
        .param("years", p.year_groups.clone())
        .param("audit", uuid::Uuid::new_v4().to_string());
    invoices::write(graph, q, a.expires, program).await
}
/// "Saya Setuju": one enrollment per child and program, inside the quota and
/// only against the terms the parent was shown.
pub async fn enroll(
    graph: &Graph,
    a: &auth::Parent,
    c: &Child,
    p: &Program,
    terms_version: &str,
    today: chrono::NaiveDate,
) -> Result<Schedule> {
    let early = early_bird_open(p.input.early_bird_until.as_deref(), today);
    let (scheme, items, discount) = if early {
        (
            "early_bird",
            &p.input.early_bird_installments,
            p.input.early_bird_discount,
        )
    } else {
        ("regular", &p.input.installments, 0)
    };
    let gross = schedule_total(items);
    let body = format!("MATCH (p:OverseasProgram {{id:$program,tenant_id:$tenant,school_id:$school,status:'open'}}) SET p._lock=coalesce(p._lock,0)+1 WITH p WHERE p.terms_version=$terms AND p.version=$program_version AND COUNT {{ (p)<-[:JOINED]-(:OverseasEnrollment) }} < $quota AND NOT EXISTS {{ MATCH (p)<-[:JOINED]-(:OverseasEnrollment {{enrolled_student_id:$enrolled}}) }} AND datetime.realtime().epochSeconds < $expires CREATE (o:OverseasEnrollment {{id:$id,key:$key,category:'overseas_program',program_id:p.id,tenant_id:$tenant,school_id:$school,student_id:$student,enrolled_student_id:$enrolled,payer_user_id:$payer,student_name:$name,scheme:$scheme,gross:$gross,discount:$discount,payable:$payable,installments_json:$installments,issue_lead_days:$lead,terms_version:$terms,accepted_by:$payer,accepted_at:datetime(),version:1,created_at:datetime()}})-[:JOINED]->(p) CREATE (:SchoolBillingAudit {{id:$audit,subject:'overseas_enrollment',ref_id:o.id,actor_id:$payer,action:'terms_accepted',version:1,payload:$terms,created_at:datetime()}}) {SCHEDULE}");
    let q = query(&body)
        .param("program", p.id.clone())
        .param("program_version", p.input.version)
        .param("tenant", c.tenant_id.clone())
        .param("school", c.school_id.clone())
        .param("terms", terms_version.to_string())
        .param("quota", p.input.quota)
        .param("enrolled", c.enrolled_student_id.clone())
        .param("expires", a.expires)
        .param("id", format!("OVSE-{}", uuid::Uuid::new_v4()))
        .param("key", format!("{}|{}", p.id, c.enrolled_student_id))
        .param("student", c.student_id.clone())
        .param("payer", a.subject.clone())
        .param("name", c.name.clone())
        .param("scheme", scheme)
        .param("gross", gross)
        .param("discount", discount)
        .param("payable", gross - discount)
        .param(
            "installments",
            serde_json::to_string(items).map_err(unavailable)?,
        )
        .param("lead", p.input.issue_lead_days)
        .param("audit", uuid::Uuid::new_v4().to_string());
    invoices::write(graph, q, a.expires, schedule).await
}
