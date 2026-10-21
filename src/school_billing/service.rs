//! Use cases: what the Payment tab shows, and issuing whatever is due.
use super::{
    model::*, repository as repo, repository::Bill, repository::Child, repository::Schedule,
};
use crate::school_invoice::{
    model::Issue,
    repository::{self as invoices, Error, Guard, Line, Result},
};
use chrono::NaiveDate;
use neo4rs::Graph;
use serde::Serialize;

/// Open (unpaid) top-up invoices a child may have at once.
pub const MAX_OPEN_TOP_UPS: usize = 3;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionView {
    pub months: i32,
    pub discount_bps: i64,
    pub gross: i64,
    pub amount: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthlyFeeView {
    pub monthly_amount: i64,
    pub options: Vec<OptionView>,
    pub selection: Option<repo::Selection>,
    /// The period cannot change while a monthly-fee invoice is open.
    pub locked: bool,
    pub bills: Vec<Bill>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    pub paid: i64,
    pub billed: i64,
    pub remaining: i64,
    pub bills: Vec<Bill>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramView {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub year_groups: Vec<String>,
    pub quota: i64,
    pub participants: i64,
    pub early_bird_until: Option<String>,
    pub early_bird_open: bool,
    pub status: String,
    pub enrollment: Option<ScheduleView>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditView {
    pub balance: i64,
    pub bills: Vec<Bill>,
}
/// One academic year of a child's Capital Levy: the year of entry paid in the
/// admissions offer, or a year billed through the Payment tab.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevyYearView {
    pub academic_year: String,
    pub payable: i64,
    pub paid: i64,
    /// `admission` (paid with the offer) or `billing`.
    pub source: &'static str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildView {
    #[serde(flatten)]
    pub child: Child,
    pub monthly_fee: Option<MonthlyFeeView>,
    /// The newest billed academic year (what the card and detail show).
    pub capital_levy: Option<ScheduleView>,
    /// Every academic year, newest first, including the year of entry.
    pub capital_levy_years: Vec<LevyYearView>,
    pub overseas: Vec<ProgramView>,
    pub credit: CreditView,
}

impl ScheduleView {
    pub fn of(schedule: Schedule, all: &[Bill]) -> Self {
        schedule_view(schedule, all)
    }
}
fn schedule_view(schedule: Schedule, all: &[Bill]) -> ScheduleView {
    let bills: Vec<Bill> = all
        .iter()
        .filter(|b| b.category == schedule.category && b.ref_id == schedule.id)
        .cloned()
        .collect();
    let paid: i64 = bills.iter().filter(|b| b.paid()).map(|b| b.amount).sum();
    let billed: i64 = bills.iter().map(|b| b.amount).sum();
    ScheduleView {
        remaining: (schedule.payable - paid).max(0),
        schedule,
        paid,
        billed,
        bills,
    }
}

pub async fn summary(graph: &Graph, c: Child) -> Result<ChildView> {
    let all = repo::bills(graph, &c.payer_user_id, &c.enrolled_student_id).await?;
    let monthly_fee = match repo::plan_for(graph, &c).await? {
        None => None,
        Some(plan) => {
            let bills: Vec<Bill> = all
                .iter()
                .filter(|b| b.category == "monthly_fee")
                .cloned()
                .collect();
            Some(MonthlyFeeView {
                monthly_amount: plan.input.monthly_amount,
                options: plan
                    .input
                    .options
                    .iter()
                    .map(|o| {
                        let (gross, _, amount) = period_amount(plan.input.monthly_amount, o);
                        OptionView {
                            months: o.months,
                            discount_bps: o.discount_bps,
                            gross,
                            amount,
                        }
                    })
                    .collect(),
                selection: repo::selection(graph, &c).await?,
                locked: bills.iter().any(Bill::open),
                bills,
            })
        }
    };
    let mut levies: Vec<ScheduleView> = repo::capital_levies_for(graph, &c)
        .await?
        .into_iter()
        .map(|s| schedule_view(s, &all))
        .collect();
    let mut capital_levy_years: Vec<LevyYearView> = levies
        .iter()
        .map(|v| LevyYearView {
            academic_year: v.schedule.academic_year.clone(),
            payable: v.schedule.payable,
            paid: v.paid,
            source: "billing",
        })
        .collect();
    if let Some(admission) = repo::admission_paid(graph, &c).await? {
        if let Some(amount) = admission.capital_levy {
            if !capital_levy_years
                .iter()
                .any(|y| y.academic_year == admission.academic_year)
            {
                capital_levy_years.push(LevyYearView {
                    academic_year: admission.academic_year,
                    payable: amount,
                    paid: amount,
                    source: "admission",
                });
            }
        }
    }
    capital_levy_years.sort_by(|a, b| b.academic_year.cmp(&a.academic_year));
    let capital_levy = if levies.is_empty() {
        None
    } else {
        Some(levies.remove(0))
    };
    let mut joined = repo::enrollments(graph, &c).await?;
    let today = today();
    let overseas = repo::programs_for(graph, &c)
        .await?
        .into_iter()
        .map(|p| {
            let enrollment = joined
                .iter()
                .position(|e| e.program_id == p.id)
                .map(|i| schedule_view(joined.remove(i), &all));
            ProgramView {
                early_bird_open: early_bird_open(p.input.early_bird_until.as_deref(), today),
                id: p.id,
                title: p.input.title,
                summary: p.input.summary,
                year_groups: p.input.year_groups,
                quota: p.input.quota,
                participants: p.participants,
                early_bird_until: p.input.early_bird_until,
                status: p.input.status,
                enrollment,
            }
        })
        .collect();
    let credit_bills: Vec<Bill> = all
        .iter()
        .filter(|b| b.category == "student_credit")
        .cloned()
        .collect();
    Ok(ChildView {
        credit: CreditView {
            balance: credit_bills
                .iter()
                .filter(|b| b.paid())
                .map(|b| b.amount)
                .sum(),
            bills: credit_bills,
        },
        child: c,
        monthly_fee,
        capital_levy,
        capital_levy_years,
        overseas,
    })
}

fn issue(
    c: &Child,
    key: String,
    period: String,
    description: &str,
    amount: i64,
    due: NaiveDate,
) -> Issue {
    Issue {
        idempotency_key: key,
        student_id: c.student_id.clone(),
        payer_user_id: c.payer_user_id.clone(),
        period,
        description: description.into(),
        amount,
        currency: "IDR".into(),
        due_date: due.format("%Y-%m-%d").to_string(),
        bank_account_id: String::new(),
    }
}

/// Issues the child's next monthly period if its window is open.
pub async fn issue_monthly(graph: &Graph, c: &Child, today: NaiveDate) -> Result<Option<Bill>> {
    let (Some(plan), Some(selection)) = (
        repo::plan_for(graph, c).await?,
        repo::selection(graph, c).await?,
    ) else {
        return Ok(None);
    };
    let Some(option) = plan
        .input
        .options
        .iter()
        .find(|o| o.months == selection.months)
    else {
        return Ok(None);
    };
    let all = repo::child_bills(graph, &c.enrolled_student_id).await?;
    let monthly: Vec<&Bill> = all.iter().filter(|b| b.category == "monthly_fee").collect();
    // The month paid with the admissions offer counts as billed.
    let admitted = repo::admission_paid(graph, c)
        .await?
        .and_then(|a| a.monthly_fee_month);
    let last_end = monthly
        .iter()
        .filter_map(|b| Month::parse(&b.period_start).map(|m| m.plus(b.months as i32 - 1)))
        .chain(admitted)
        .max();
    let Some(bill) = monthly_bill(
        plan.input.monthly_amount,
        option,
        plan.input.due_day,
        plan.input.issue_lead_days,
        last_end,
        monthly.iter().any(|b| b.open()),
        today,
    ) else {
        return Ok(None);
    };
    let input = issue(
        c,
        format!("monthly.{}.{}", c.enrolled_student_id, bill.start.key()),
        period_label(bill.start, bill.months),
        &format!("Advance Monthly Fee ({} bulan)", bill.months),
        bill.amount,
        bill.due,
    );
    let line = Line {
        category: "monthly_fee".into(),
        kind: "period".into(),
        ref_id: format!(
            "{}|{}|{}",
            plan.input.tenant_id, plan.input.school_id, plan.input.year_group
        ),
        period_start: bill.start.key(),
        months: i64::from(bill.months),
        installment: -1,
        gross: bill.gross,
        discount: bill.discount,
        guard: Guard::NoOpenMonthly,
    };
    issued(graph, c, input, line).await.map(Some)
}

/// A retried parent request returns what the first one issued.
async fn replayed(
    graph: &Graph,
    c: &Child,
    key: &str,
) -> std::result::Result<Option<Bill>, super::Failure> {
    let Some(id) = repo::issued_by_key(graph, c, key)
        .await
        .map_err(super::owner_error)?
    else {
        return Ok(None);
    };
    repo::bills(graph, &c.payer_user_id, &c.enrolled_student_id)
        .await
        .map_err(super::owner_error)
        .map(|all| all.into_iter().find(|b| b.invoice_id == id))
}

async fn issued(graph: &Graph, c: &Child, input: Issue, line: Line) -> Result<Bill> {
    let invoice = invoices::issue_system(graph, input, &line).await?;
    repo::bills(graph, &c.payer_user_id, &c.enrolled_student_id)
        .await?
        .into_iter()
        .find(|b| b.invoice_id == invoice.snapshot.id)
        .ok_or(Error::Unavailable)
}

fn label(category: &str) -> &'static str {
    if category == "capital_levy" {
        "Capital Levy"
    } else {
        "Overseas Program"
    }
}
fn prefix(category: &str) -> &'static str {
    if category == "capital_levy" {
        "levy"
    } else {
        "ovs"
    }
}

/// Issues the next installment of a schedule whose window is open.
pub async fn issue_installment(
    graph: &Graph,
    c: &Child,
    s: &Schedule,
    today: NaiveDate,
) -> Result<Option<Bill>> {
    let all = repo::child_bills(graph, &c.enrolled_student_id).await?;
    let mine: Vec<&Bill> = all
        .iter()
        .filter(|b| b.category == s.category && b.ref_id == s.id)
        .collect();
    let billed: i64 = mine.iter().map(|b| b.amount).sum();
    let issued_at: Vec<usize> = mine
        .iter()
        .filter_map(|b| usize::try_from(b.installment).ok())
        .collect();
    let Some((k, amount, due)) = next_installment(
        &s.installments,
        s.payable,
        billed,
        &issued_at,
        s.issue_lead_days,
        today,
    ) else {
        return Ok(None);
    };
    let item = &s.installments[k];
    let input = issue(
        c,
        format!("{}.{}.{k}", prefix(&s.category), s.id),
        item.label.clone(),
        &format!("{} — {}", label(&s.category), item.label),
        amount,
        due,
    );
    let line = Line {
        category: s.category.clone(),
        kind: "installment".into(),
        ref_id: s.id.clone(),
        installment: k as i64,
        gross: amount,
        guard: Guard::Billed(billed),
        ..Line::default()
    };
    issued(graph, c, input, line).await.map(Some)
}

/// Everything due for one child; failures of one schedule do not stop the
/// others. Returns how many invoices were issued.
pub async fn issue_due(graph: &Graph, c: &Child, today: NaiveDate) -> Result<usize> {
    let mut count = 0;
    match issue_monthly(graph, c, today).await {
        Ok(Some(_)) => count += 1,
        Ok(None) | Err(Error::Conflict) | Err(Error::Denied) => {}
        Err(e) => return Err(e),
    }
    ensure_levies(graph, c).await?;
    let mut schedules = repo::enrollments(graph, c).await?;
    schedules.extend(repo::capital_levies_for(graph, c).await?);
    for s in &schedules {
        match issue_installment(graph, c, s, today).await {
            Ok(Some(_)) => count += 1,
            Ok(None) | Err(Error::Conflict) | Err(Error::Denied) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(count)
}

/// Creates the child's Capital Levy for every active plan year that the
/// admissions offer did not already pay.
pub async fn ensure_levies(graph: &Graph, c: &Child) -> Result<()> {
    let admitted = repo::admission_paid(graph, c)
        .await?
        .filter(|a| a.capital_levy.is_some())
        .map(|a| a.academic_year);
    for plan in repo::levy_plans_for(graph, c).await? {
        if admitted.as_deref() != Some(plan.input.academic_year.as_str()) {
            repo::ensure_levy(graph, c, &plan.input).await?;
        }
    }
    Ok(())
}

/// A parent-chosen extra payment towards a schedule.
pub async fn deposit(
    graph: &Graph,
    c: &Child,
    s: &Schedule,
    amount: i64,
    request: &str,
) -> std::result::Result<Bill, super::Failure> {
    let key = format!("{}-deposit.{}.{request}", prefix(&s.category), s.id);
    if let Some(bill) = replayed(graph, c, &key).await? {
        return Ok(bill);
    }
    let all = repo::child_bills(graph, &c.enrolled_student_id)
        .await
        .map_err(super::owner_error)?;
    let billed: i64 = all
        .iter()
        .filter(|b| b.category == s.category && b.ref_id == s.id)
        .map(|b| b.amount)
        .sum();
    if !deposit_ok(amount, s.payable, billed) {
        return Err(super::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "AMOUNT_OUT_OF_RANGE",
        ));
    }
    let input = issue(
        c,
        key,
        "Deposit".into(),
        &format!("{} — Deposit", label(&s.category)),
        amount,
        today(),
    );
    let line = Line {
        category: s.category.clone(),
        kind: "deposit".into(),
        ref_id: s.id.clone(),
        installment: -1,
        gross: amount,
        guard: Guard::Billed(billed),
        ..Line::default()
    };
    issued(graph, c, input, line)
        .await
        .map_err(super::owner_error)
}

pub async fn top_up(
    graph: &Graph,
    c: &Child,
    amount: i64,
    request: &str,
) -> std::result::Result<Bill, super::Failure> {
    let key = format!("credit.{}.{request}", c.enrolled_student_id);
    if let Some(bill) = replayed(graph, c, &key).await? {
        return Ok(bill);
    }
    if !(MIN_DEPOSIT..=MAX_TOP_UP).contains(&amount) {
        return Err(super::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "AMOUNT_OUT_OF_RANGE",
        ));
    }
    // A few open top-ups are enough; more would only be unpaid invoices.
    let open = repo::bills(graph, &c.payer_user_id, &c.enrolled_student_id)
        .await
        .map_err(super::owner_error)?
        .iter()
        .filter(|b| b.category == "student_credit" && b.open())
        .count();
    if open >= MAX_OPEN_TOP_UPS {
        return Err(super::coded(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "OPEN_INVOICE_LIMIT",
        ));
    }
    let input = issue(
        c,
        key,
        "Top Up".into(),
        "Student Credit — Top Up",
        amount,
        today(),
    );
    let line = Line {
        category: "student_credit".into(),
        kind: "top_up".into(),
        ref_id: c.enrolled_student_id.clone(),
        installment: -1,
        gross: amount,
        ..Line::default()
    };
    issued(graph, c, input, line)
        .await
        .map_err(super::owner_error)
}
