//! Pure billing rules (ADR 0014). No I/O: amounts are IDR major units,
//! months are `YYYY-MM`, dates are school-local `YYYY-MM-DD`.
use crate::school_invoice::model::{identifier, text};
use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

pub const MAX_AMOUNT: i64 = 1_000_000_000_000;
pub const MIN_DEPOSIT: i64 = 100_000;
pub const MAX_TOP_UP: i64 = 100_000_000;

/// School-local "today" (WIB). Billing dates are calendar dates in Jakarta.
pub fn today() -> NaiveDate {
    (chrono::Utc::now() + Duration::hours(7)).date_naive()
}

pub fn date(v: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(v, "%Y-%m-%d")
        .ok()
        .filter(|d| d.format("%Y-%m-%d").to_string() == v)
}

/// A calendar month as `year * 12 + (month - 1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Month(pub i32);
impl Month {
    pub fn of(d: NaiveDate) -> Self {
        Month(d.year() * 12 + d.month0() as i32)
    }
    pub fn parse(v: &str) -> Option<Self> {
        let d = NaiveDate::parse_from_str(&format!("{v}-01"), "%Y-%m-%d").ok()?;
        (d.format("%Y-%m").to_string() == v).then(|| Month::of(d))
    }
    pub fn year(self) -> i32 {
        self.0.div_euclid(12)
    }
    pub fn month(self) -> u32 {
        self.0.rem_euclid(12) as u32 + 1
    }
    pub fn plus(self, n: i32) -> Self {
        Month(self.0 + n)
    }
    pub fn key(self) -> String {
        format!("{:04}-{:02}", self.year(), self.month())
    }
    pub fn day(self, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year(), self.month(), day.clamp(1, 28)).expect("day 1..=28")
    }
}

const MONTHS_ID: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des",
];
/// Short invoice period text, e.g. `Jan–Mar 2026` or `Nov 2026–Jan 2027`.
pub fn period_label(start: Month, months: i32) -> String {
    let end = start.plus(months - 1);
    let name = |m: Month| MONTHS_ID[(m.month() - 1) as usize];
    if months == 1 {
        format!("{} {}", name(start), start.year())
    } else if start.year() == end.year() {
        format!("{}–{} {}", name(start), name(end), end.year())
    } else {
        format!(
            "{} {}–{} {}",
            name(start),
            start.year(),
            name(end),
            end.year()
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeriodOption {
    pub months: i32,
    /// Discount in basis points (300 = 3%).
    pub discount_bps: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeePlanInput {
    pub tenant_id: String,
    pub school_id: String,
    /// Section year group this plan prices, or `*` for the whole school.
    pub year_group: String,
    pub monthly_amount: i64,
    pub options: Vec<PeriodOption>,
    pub due_day: u32,
    pub issue_lead_days: i64,
    pub active: bool,
    /// Optimistic version the caller last read (0 to create).
    pub version: i64,
}
impl FeePlanInput {
    pub fn valid(&self) -> bool {
        identifier(&self.tenant_id)
            && identifier(&self.school_id)
            && (self.year_group == "*" || text(&self.year_group, 32))
            && self.monthly_amount > 0
            && self.monthly_amount <= MAX_AMOUNT / 12
            && !self.options.is_empty()
            && self.options.len() <= 4
            && self
                .options
                .iter()
                .all(|o| matches!(o.months, 1 | 3 | 6 | 12) && (0..=5000).contains(&o.discount_bps))
            && {
                let mut m: Vec<_> = self.options.iter().map(|o| o.months).collect();
                m.sort_unstable();
                m.dedup();
                m.len() == self.options.len()
            }
            && (1..=28).contains(&self.due_day)
            && (0..=60).contains(&self.issue_lead_days)
            && self.version >= 0
    }
}

/// Gross, discount and net for one period.
pub fn period_amount(monthly: i64, option: &PeriodOption) -> (i64, i64, i64) {
    let gross = monthly.saturating_mul(option.months as i64);
    let discount = gross.saturating_mul(option.discount_bps) / 10_000;
    (gross, discount, gross - discount)
}

/// Where the next period starts: right after the last billed month, never
/// before the current month.
pub fn next_period_start(last_billed_end: Option<Month>, today: NaiveDate) -> Month {
    let now = Month::of(today);
    match last_billed_end {
        Some(end) if end.plus(1) > now => end.plus(1),
        _ => now,
    }
}

/// The next monthly-fee bill to issue, if its issue window is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthlyBill {
    pub start: Month,
    pub months: i32,
    pub gross: i64,
    pub discount: i64,
    pub amount: i64,
    pub due: NaiveDate,
}
pub fn monthly_bill(
    monthly: i64,
    option: &PeriodOption,
    due_day: u32,
    lead_days: i64,
    last_billed_end: Option<Month>,
    has_open_invoice: bool,
    today: NaiveDate,
) -> Option<MonthlyBill> {
    if has_open_invoice {
        return None;
    }
    let start = next_period_start(last_billed_end, today);
    let due = start.day(due_day);
    // A first period that starts this month is billed at once, even if its
    // due day already passed; later periods wait for their lead window.
    let first_now = last_billed_end.is_none() && start == Month::of(today);
    if !first_now && today < due - Duration::days(lead_days) {
        return None;
    }
    let (gross, discount, amount) = period_amount(monthly, option);
    Some(MonthlyBill {
        start,
        months: option.months,
        gross,
        discount,
        amount,
        due: if due < today { today } else { due },
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installment {
    pub label: String,
    pub amount: i64,
    pub due_date: String,
}
pub fn valid_schedule(items: &[Installment]) -> bool {
    !items.is_empty()
        && items.len() <= 24
        && items.iter().all(|i| {
            text(&i.label, 64)
                && i.amount > 0
                && i.amount <= MAX_AMOUNT
                && date(&i.due_date).is_some()
        })
        && items.windows(2).all(|w| w[0].due_date <= w[1].due_date)
        && items
            .iter()
            .map(|i| i.amount)
            .try_fold(0i64, i64::checked_add)
            .is_some_and(|s| s <= MAX_AMOUNT)
}
pub fn schedule_total(items: &[Installment]) -> i64 {
    items.iter().map(|i| i.amount).sum()
}

/// Installment `k` (0-based) bills what the schedule has made due through `k`
/// that is not billed yet, capped by the payable total (a discount or an
/// early deposit shrinks or skips the last installments).
pub fn installment_bill(items: &[Installment], k: usize, payable: i64, billed: i64) -> i64 {
    let through: i64 = items.iter().take(k + 1).map(|i| i.amount).sum();
    (through.min(payable) - billed).max(0)
}

/// The next installment to issue: the first one whose window opened and
/// that still has something to bill. Returns `(index, amount, due)`.
pub fn next_installment(
    items: &[Installment],
    payable: i64,
    billed: i64,
    issued: &[usize],
    lead_days: i64,
    today: NaiveDate,
) -> Option<(usize, i64, NaiveDate)> {
    for (k, item) in items.iter().enumerate() {
        if issued.contains(&k) {
            continue;
        }
        let due = date(&item.due_date)?;
        if today < due - Duration::days(lead_days) {
            return None;
        }
        let amount = installment_bill(items, k, payable, billed);
        if amount > 0 {
            return Some((k, amount, if due < today { today } else { due }));
        }
    }
    None
}

/// A parent-chosen deposit must leave nothing negative and be worth a bill.
pub fn deposit_ok(amount: i64, payable: i64, billed: i64) -> bool {
    amount >= MIN_DEPOSIT.min(payable - billed).max(1) && amount <= payable - billed
}

/// The Capital Levy of one academic year for a school's year group (or `*`):
/// every active enrolled child in it owes this schedule for that year.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapitalLevyPlanInput {
    pub tenant_id: String,
    pub school_id: String,
    pub year_group: String,
    /// Start year of the academic year: `2027` is 2027/2028 (the admissions offer's `academic_year`).
    pub academic_year: String,
    pub installments: Vec<Installment>,
    pub issue_lead_days: i64,
    pub active: bool,
    pub version: i64,
}
impl CapitalLevyPlanInput {
    pub fn valid(&self) -> bool {
        identifier(&self.tenant_id)
            && identifier(&self.school_id)
            && (self.year_group == "*" || text(&self.year_group, 32))
            && academic_year(&self.academic_year)
            && valid_schedule(&self.installments)
            && (0..=60).contains(&self.issue_lead_days)
            && self.version >= 0
    }
}
pub fn academic_year(v: &str) -> bool {
    v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()) && ("2000".."2100").contains(&v)
}

/// What the admissions offer already paid for a child: the Capital Levy of
/// the year of entry and the month of monthly fee it included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdmissionPaid {
    pub academic_year: String,
    pub capital_levy: Option<i64>,
    pub monthly_fee_month: Option<Month>,
}
/// Academic years start in July (user, 2026-10-21): `2026` is 2026/2027.
pub const ACADEMIC_YEAR_START_MONTH: u32 = 7;
/// The first month of an academic year (`2026` → 2026-07).
pub fn academic_year_start(year: &str) -> Option<Month> {
    let start: i32 = year.parse().ok().filter(|_| academic_year(year))?;
    Some(Month(start * 12 + ACADEMIC_YEAR_START_MONTH as i32 - 1))
}
/// Reads a paid offer's pricing snapshot (`offer-pricing-v1`). The levy counts
/// only when the offer was paid in full (nothing left on an installment plan).
pub fn admission_paid(academic_year_value: &str, pricing_json: &str) -> Option<AdmissionPaid> {
    let v: serde_json::Value = serde_json::from_str(pricing_json).ok()?;
    if v.get("currency")?.as_str()? != "IDR" || !academic_year(academic_year_value) {
        return None;
    }
    let settled = v.get("remainingBalance").and_then(|r| r.as_i64()) == Some(0);
    let lines = v.get("lineItems")?.as_array()?;
    let code = |c: &str| {
        lines
            .iter()
            .find(|l| l.get("code").and_then(|x| x.as_str()) == Some(c))
    };
    Some(AdmissionPaid {
        academic_year: academic_year_value.to_string(),
        capital_levy: code("CAPITAL_LEVY")
            .and_then(|l| l.get("amount")?.as_i64())
            .filter(|a| *a > 0 && settled),
        // The offer's monthly fee is the academic year's first month; its
        // label is fixed admissions text and does not name the month.
        monthly_fee_month: code("MONTHLY_FEE")
            .and_then(|l| l.get("amount")?.as_i64())
            .filter(|a| *a > 0)
            .and_then(|_| academic_year_start(academic_year_value)),
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OverseasProgramInput {
    pub tenant_id: String,
    pub school_id: String,
    pub title: String,
    pub summary: String,
    pub terms: String,
    pub year_groups: Vec<String>,
    pub quota: i64,
    /// Last day the early-bird scheme applies; `None` disables it.
    pub early_bird_until: Option<String>,
    pub early_bird_discount: i64,
    pub early_bird_installments: Vec<Installment>,
    pub installments: Vec<Installment>,
    pub issue_lead_days: i64,
    pub status: String,
    pub version: i64,
}
impl OverseasProgramInput {
    pub fn valid(&self) -> bool {
        identifier(&self.tenant_id)
            && identifier(&self.school_id)
            && text(&self.title, 128)
            && text(&self.summary, 512)
            && terms_text(&self.terms)
            && self.year_groups.len() <= 20
            && self.year_groups.iter().all(|y| text(y, 32))
            && (1..=10_000).contains(&self.quota)
            && valid_schedule(&self.installments)
            && match &self.early_bird_until {
                None => self.early_bird_installments.is_empty() && self.early_bird_discount == 0,
                Some(d) => {
                    date(d).is_some()
                        && valid_schedule(&self.early_bird_installments)
                        && (0..schedule_total(&self.early_bird_installments))
                            .contains(&self.early_bird_discount)
                }
            }
            && (0..=60).contains(&self.issue_lead_days)
            && matches!(self.status.as_str(), "draft" | "open" | "closed")
            && self.version >= 0
    }
    pub fn terms_version(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(self.terms.as_bytes()))[..16].to_string()
    }
}

/// Terms are multi-line: line breaks and tabs are allowed, other control
/// characters are not.
pub fn terms_text(v: &str) -> bool {
    !v.trim().is_empty()
        && v.len() <= 40_000
        && !v
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

/// Which scheme a parent joining today gets.
pub fn early_bird_open(until: Option<&str>, today: NaiveDate) -> bool {
    until.and_then(date).is_some_and(|d| today <= d)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(v: &str) -> NaiveDate {
        date(v).unwrap()
    }
    fn opt(months: i32, bps: i64) -> PeriodOption {
        PeriodOption {
            months,
            discount_bps: bps,
        }
    }
    fn inst(label: &str, amount: i64, due: &str) -> Installment {
        Installment {
            label: label.into(),
            amount,
            due_date: due.into(),
        }
    }

    #[test]
    fn months_and_labels() {
        let m = Month::parse("2026-11").unwrap();
        assert_eq!(m.plus(2).key(), "2027-01");
        assert!(Month::parse("2026-13").is_none());
        assert!(Month::parse("2026-1").is_none());
        assert_eq!(
            period_label(Month::parse("2026-01").unwrap(), 3),
            "Jan–Mar 2026"
        );
        assert_eq!(period_label(m, 3), "Nov 2026–Jan 2027");
        assert_eq!(period_label(m, 1), "Nov 2026");
        assert_eq!(m.day(31), d("2026-11-28"));
    }

    #[test]
    fn discount_rounds_toward_the_parent() {
        assert_eq!(
            period_amount(7_000_000, &opt(3, 0)),
            (21_000_000, 0, 21_000_000)
        );
        assert_eq!(
            period_amount(7_000_000, &opt(6, 300)),
            (42_000_000, 1_260_000, 40_740_000)
        );
        assert_eq!(period_amount(333, &opt(3, 300)), (999, 29, 970));
    }

    #[test]
    fn first_period_starts_now_and_bills_at_once() {
        let bill =
            monthly_bill(7_000_000, &opt(3, 0), 10, 14, None, false, d("2026-10-21")).unwrap();
        assert_eq!(bill.start.key(), "2026-10");
        assert_eq!(bill.due, d("2026-10-21"), "a past due day becomes today");
        assert_eq!(bill.amount, 21_000_000);
    }

    #[test]
    fn later_periods_wait_for_their_lead_window() {
        let last = Month::parse("2026-12");
        assert!(monthly_bill(1, &opt(3, 0), 10, 14, last, false, d("2026-12-26")).is_none());
        let bill = monthly_bill(1, &opt(3, 0), 10, 14, last, false, d("2026-12-27")).unwrap();
        assert_eq!(
            (bill.start.key(), bill.due),
            ("2027-01".into(), d("2027-01-10"))
        );
        assert!(monthly_bill(1, &opt(3, 0), 10, 14, last, true, d("2026-12-27")).is_none());
    }

    #[test]
    fn a_gap_restarts_at_the_current_month() {
        let bill = monthly_bill(
            1,
            &opt(1, 0),
            5,
            0,
            Month::parse("2026-03"),
            false,
            d("2026-10-21"),
        )
        .unwrap();
        assert_eq!(bill.start.key(), "2026-10");
    }

    #[test]
    fn plan_validation() {
        let mut p = FeePlanInput {
            tenant_id: "t".into(),
            school_id: "s".into(),
            year_group: "*".into(),
            monthly_amount: 7_000_000,
            options: vec![opt(3, 0), opt(6, 300), opt(12, 600)],
            due_day: 10,
            issue_lead_days: 14,
            active: true,
            version: 0,
        };
        assert!(p.valid());
        p.options.push(opt(3, 100));
        assert!(!p.valid(), "duplicate months");
        p.options.pop();
        p.due_day = 31;
        assert!(!p.valid());
        p.due_day = 10;
        p.options[1].months = 5;
        assert!(!p.valid());
    }

    #[test]
    fn deposits_shrink_later_installments() {
        let items = [
            inst("Capital Levy I", 75, "2026-07-01"),
            inst("Capital Levy II", 75, "2026-10-14"),
        ];
        assert_eq!(installment_bill(&items, 0, 150, 0), 75);
        assert_eq!(installment_bill(&items, 1, 150, 75), 75);
        // a 50 deposit after installment I leaves 25 for II
        assert_eq!(installment_bill(&items, 1, 150, 125), 25);
        // discount caps the payable total
        assert_eq!(installment_bill(&items, 1, 135, 75), 60);
        assert_eq!(installment_bill(&items, 1, 150, 150), 0);
    }

    #[test]
    fn next_installment_respects_window_and_issued() {
        let items = [inst("I", 75, "2026-07-01"), inst("II", 75, "2026-11-14")];
        assert_eq!(
            next_installment(&items, 150, 0, &[], 14, d("2026-10-21")),
            Some((0, 75, d("2026-10-21")))
        );
        assert_eq!(
            next_installment(&items, 150, 75, &[0], 14, d("2026-10-21")),
            None
        );
        assert_eq!(
            next_installment(&items, 150, 75, &[0], 14, d("2026-10-31")),
            Some((1, 75, d("2026-11-14")))
        );
        // fully prepaid by deposits: nothing more to issue
        assert_eq!(
            next_installment(&items, 150, 150, &[0], 14, d("2026-11-10")),
            None
        );
    }

    #[test]
    fn deposit_bounds() {
        assert!(deposit_ok(100_000, 1_000_000, 0));
        assert!(!deposit_ok(99_999, 1_000_000, 0));
        assert!(!deposit_ok(1_000_001, 1_000_000, 0));
        // a remainder under the minimum can still be settled exactly
        assert!(deposit_ok(50_000, 1_000_000, 950_000));
        assert!(!deposit_ok(1, 1_000_000, 1_000_000));
    }

    #[test]
    fn overseas_validation_and_scheme() {
        let mut p = OverseasProgramInput {
            tenant_id: "t".into(),
            school_id: "s".into(),
            title: "Overseas 2027".into(),
            summary: "Study trip".into(),
            terms: "1. Definisi".into(),
            year_groups: vec!["10".into()],
            quota: 30,
            early_bird_until: Some("2027-03-12".into()),
            early_bird_discount: 15,
            early_bird_installments: vec![
                inst("Cicilan 1", 50, "2027-01-12"),
                inst("Cicilan 2", 75, "2027-03-12"),
            ],
            installments: vec![inst("Cicilan 1", 125, "2027-01-12")],
            issue_lead_days: 14,
            status: "open".into(),
            version: 0,
        };
        assert!(p.valid());
        p.early_bird_discount = 125;
        assert!(!p.valid(), "discount must leave something to pay");
        p.early_bird_discount = 15;
        assert_eq!(p.terms_version().len(), 16);
        p.terms = "# 1. Definisi\n- Program Overseas\n\t1. Peserta".into();
        assert!(p.valid(), "terms keep their lines");
        p.terms = "bad\u{0007}bell".into();
        assert!(!p.valid());
        p.terms = "1. Definisi".into();
        assert!(early_bird_open(
            p.early_bird_until.as_deref(),
            d("2027-03-12")
        ));
        assert!(!early_bird_open(
            p.early_bird_until.as_deref(),
            d("2027-03-13")
        ));
        assert!(!early_bird_open(None, d("2027-01-01")));
    }

    #[test]
    fn admission_offer_counts_its_levy_and_month() {
        let pricing = r#"{"snapshotVersion":"offer-pricing-v1","currency":"IDR","lineItems":[{"code":"CAPITAL_LEVY","label":"Capital Levy","amount":75000000},{"code":"MONTHLY_FEE","label":"Monthly Fee - July 2027","amount":7000000}],"remainingBalance":0}"#;
        let paid = admission_paid("2026", pricing).unwrap();
        assert_eq!(paid.capital_levy, Some(75_000_000));
        assert_eq!(
            paid.monthly_fee_month.map(Month::key),
            Some("2026-07".into()),
            "July of the offer's academic year, whatever the label says"
        );
        let partial = pricing.replace("\"remainingBalance\":0", "\"remainingBalance\":5");
        assert_eq!(
            admission_paid("2026", &partial).unwrap().capital_levy,
            None,
            "unpaid installments"
        );
        assert!(admission_paid("26", pricing).is_none());
        // Paid in July 2026, period chosen in October: the first period starts in October.
        let option = PeriodOption {
            months: 3,
            discount_bps: 0,
        };
        let first = monthly_bill(
            1,
            &option,
            10,
            14,
            paid.monthly_fee_month,
            false,
            d("2026-10-21"),
        )
        .unwrap();
        assert_eq!(first.start.key(), "2026-10");
    }

    #[test]
    fn levy_plan_validation() {
        let mut p = CapitalLevyPlanInput {
            tenant_id: "t".into(),
            school_id: "s".into(),
            year_group: "year7".into(),
            academic_year: "2027".into(),
            installments: vec![inst("Capital Levy I", 75, "2027-06-10")],
            issue_lead_days: 14,
            active: true,
            version: 0,
        };
        assert!(p.valid());
        p.academic_year = "27/28".into();
        assert!(!p.valid());
    }

    #[test]
    fn schedules_must_be_ordered() {
        assert!(!valid_schedule(&[
            inst("b", 1, "2026-02-01"),
            inst("a", 1, "2026-01-01")
        ]));
        assert!(!valid_schedule(&[]));
        assert!(!valid_schedule(&[inst("a", 0, "2026-01-01")]));
    }
}
