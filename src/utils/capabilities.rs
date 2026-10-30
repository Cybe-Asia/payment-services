//! ADR 0018 slice 6: staff whose Auth roles do not open a Payment gate may still
//! pass it when SIS says they hold the matching capability.
//!
//! Roles are always checked first and keep today's answer. Only when they deny,
//! and only for Auth's `staff-api+jwt`, the gate asks SIS
//! `GET /api/v1/sis-service/authorization/held` with that same bearer. Missing
//! configuration, any SIS error, a timeout or an answer for another Staff Member
//! keeps the denial. Tokens are never logged.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

pub const PAYMENT_READ: &str = "billing.payment.read";
pub const PAYMENT_MANAGE: &str = "billing.payment.manage";
pub const SCHOOL_INVOICE_MANAGE: &str = "billing.school_invoice.manage";
pub const UPLOAD_ON_BEHALF: &str = "admissions.document.upload_on_behalf";

/// SIS origin (scheme and host, no path). Empty or unset turns the capability
/// path off, so every gate answers exactly by role.
const ORIGIN_ENV: &str = "SIS_ACCESS_ORIGIN";
const HELD_PATH: &str = "/api/v1/sis-service/authorization/held";
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BODY: usize = 64 * 1024;

/// The legacy gates a capability can open. Payment review is not split by
/// school, so these open only for a tenant-wide holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// `require_finance(approve = false)`: review queue, detail, proofs.
    FinanceView,
    /// `require_finance(approve = true)`: money-mutating review.
    FinanceApprove,
    /// `require_staff`: assisted submission of payment proof.
    AssistedSubmit,
}

impl Gate {
    pub fn finance(approve: bool) -> Self {
        if approve {
            Gate::FinanceApprove
        } else {
            Gate::FinanceView
        }
    }
    /// Any one of these codes, held for the whole tenant, opens the gate.
    pub fn codes(self) -> &'static [&'static str] {
        match self {
            Gate::FinanceView => &[PAYMENT_READ, PAYMENT_MANAGE],
            Gate::FinanceApprove => &[PAYMENT_MANAGE],
            Gate::AssistedSubmit => &[UPLOAD_ON_BEHALF],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Holding {
    #[serde(default)]
    pub tenants: Vec<String>,
    #[serde(default)]
    pub schools: Vec<HeldSchool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeldSchool {
    pub tenant_id: String,
    pub school_id: String,
}

/// SIS `held` answer (`data`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Held {
    pub staff_member_id: String,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Holding>,
}

#[derive(Deserialize)]
struct Envelope {
    data: Held,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// `SIS_ACCESS_ORIGIN` is not configured.
    Off,
    /// Transport error, timeout, non-200 status or an unreadable answer.
    Failed,
}

pub type Answer = Result<Held, Unavailable>;

impl Held {
    /// A holding counts only when the answer is about the session's own Staff Member.
    fn holding(&self, staff: &str, code: &str) -> Option<&Holding> {
        if staff.is_empty() || self.staff_member_id != staff {
            return None;
        }
        self.capabilities.get(code)
    }
    pub fn tenant_wide(&self, staff: &str, code: &str, tenant: &str) -> bool {
        self.holding(staff, code)
            .is_some_and(|h| h.tenants.iter().any(|t| t == tenant))
    }
}

/// The whole decision for a legacy gate, without I/O. `role_allows` is today's
/// answer and wins unchanged; otherwise only a tenant-wide holding of one of the
/// gate's codes, for the session's Staff Member, in this service's tenant opens
/// it. No answer (not asked, off, failed) denies.
pub fn decide(
    role_allows: bool,
    session_staff: Option<&str>,
    gate: Gate,
    tenant: &str,
    answer: Option<&Answer>,
) -> bool {
    if role_allows {
        return true;
    }
    let (Some(staff), Some(Ok(held))) = (session_staff, answer) else {
        return false;
    };
    gate.codes()
        .iter()
        .any(|code| held.tenant_wide(staff, code, tenant))
}

/// Where a school-billing capability holds, as Cypher parameters: tenants held
/// tenant-wide, and `tenant|school` keys for each school. Only identifier-shaped
/// values are kept, so a key always has exactly one separator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchoolReach {
    pub tenants: Vec<String>,
    pub schools: Vec<String>,
}

impl SchoolReach {
    pub fn any(&self) -> bool {
        !self.tenants.is_empty() || !self.schools.is_empty()
    }
}

pub fn school_reach(answer: &Answer, staff: &str, code: &str) -> SchoolReach {
    let Ok(held) = answer else {
        return SchoolReach::default();
    };
    let Some(holding) = held.holding(staff, code) else {
        return SchoolReach::default();
    };
    let mut reach = SchoolReach {
        tenants: holding
            .tenants
            .iter()
            .filter(|t| identifier(t))
            .cloned()
            .collect(),
        schools: holding
            .schools
            .iter()
            .filter(|s| identifier(&s.tenant_id) && identifier(&s.school_id))
            .map(|s| format!("{}|{}", s.tenant_id, s.school_id))
            .collect(),
    };
    reach.tenants.sort();
    reach.tenants.dedup();
    reach.schools.sort();
    reach.schools.dedup();
    reach
}

fn identifier(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn origin() -> Option<String> {
    let raw = std::env::var(ORIGIN_ENV).ok()?;
    let origin = raw.trim().trim_end_matches('/');
    if origin.starts_with("https://") || origin.starts_with("http://") {
        Some(origin.to_string())
    } else {
        None
    }
}

/// True when the capability path is configured.
pub fn enabled() -> bool {
    origin().is_some()
}

/// The tenant this Payment deployment serves (same setting as `Config::tenant_id`).
pub fn tenant() -> String {
    std::env::var("TENANT_ID").unwrap_or_else(|_| "TENANT-001".to_string())
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

/// Ask SIS which of `codes` the bearer's Staff Member holds. `bearer` must be
/// the `staff-api+jwt` the request carried.
pub async fn held(bearer: &str, codes: &[&str]) -> Answer {
    let Some(origin) = origin() else {
        return Err(Unavailable::Off);
    };
    held_from(&origin, bearer, codes).await
}

async fn held_from(origin: &str, bearer: &str, codes: &[&str]) -> Answer {
    let request = client()
        .get(format!("{origin}{HELD_PATH}"))
        .query(&[("capabilities", codes.join(","))])
        .bearer_auth(bearer)
        .send();
    let read = async {
        let response = request.await.map_err(|_| "transport")?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("status");
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err("size");
        }
        let body = response.bytes().await.map_err(|_| "body")?;
        if body.len() > MAX_BODY {
            return Err("size");
        }
        serde_json::from_slice::<Envelope>(&body)
            .map(|e| e.data)
            .map_err(|_| "parse")
    };
    match tokio::time::timeout(TIMEOUT, read).await {
        Ok(Ok(held)) => Ok(held),
        Ok(Err(reason)) => {
            tracing::warn!(
                reason,
                "SIS capability read failed; staff gate keeps its denial"
            );
            Err(Unavailable::Failed)
        }
        Err(_) => {
            tracing::warn!("SIS capability read timed out; staff gate keeps its denial");
            Err(Unavailable::Failed)
        }
    }
}

/// Test-only SIS `held` endpoint: answers with what the test sets and records
/// each call, so graph tests can prove when SIS is (not) asked.
#[cfg(test)]
pub(crate) mod stub {
    use axum::{extract::State, http::HeaderMap, http::StatusCode, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Shared {
        answer: Arc<Mutex<(u16, Value)>>,
        calls: Arc<Mutex<Vec<(String, String)>>>,
    }
    /// `(code, tenant-wide tenants, (tenant, school) pairs)`.
    pub type Entry<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);
    pub struct Sis {
        pub origin: String,
        shared: Shared,
    }
    impl Sis {
        pub async fn start() -> Self {
            let shared = Shared::default();
            *shared.answer.lock().unwrap() = (503, json!({}));
            let app = Router::new()
                .route(super::HELD_PATH, get(held))
                .with_state(shared.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            Self { origin, shared }
        }
        /// `held` answer for `staff`: `(code, tenants, (tenant, school) pairs)`.
        pub fn holds(&self, staff: &str, entries: &[Entry]) {
            let caps: serde_json::Map<String, Value> = entries
                .iter()
                .map(|(code, tenants, schools)| {
                    let schools: Vec<Value> = schools
                        .iter()
                        .map(|(t, s)| json!({"tenantId": t, "schoolId": s}))
                        .collect();
                    (
                        code.to_string(),
                        json!({"tenants": tenants, "schools": schools}),
                    )
                })
                .collect();
            self.reply(
                200,
                json!({"data":{"staffMemberId":staff,"cacheSeconds":60,"capabilities":caps}}),
            );
        }
        pub fn reply(&self, status: u16, body: Value) {
            *self.shared.answer.lock().unwrap() = (status, body);
        }
        /// `(Authorization header, capabilities query)` of each call so far.
        pub fn calls(&self) -> Vec<(String, String)> {
            self.shared.calls.lock().unwrap().clone()
        }
        pub fn forget(&self) {
            self.shared.calls.lock().unwrap().clear();
        }
    }
    async fn held(
        State(shared): State<Shared>,
        headers: HeaderMap,
        axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    ) -> (StatusCode, Json<Value>) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        shared
            .calls
            .lock()
            .unwrap()
            .push((auth, q.get("capabilities").cloned().unwrap_or_default()));
        let (status, body) = shared.answer.lock().unwrap().clone();
        (StatusCode::from_u16(status).unwrap(), Json(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "TENANT-001";
    const STAFF: &str = "staff-1";

    use stub::Entry;

    fn held(entries: &[Entry]) -> Answer {
        Ok(Held {
            staff_member_id: STAFF.into(),
            capabilities: entries
                .iter()
                .map(|(code, tenants, schools)| {
                    (
                        code.to_string(),
                        Holding {
                            tenants: tenants.iter().map(|t| t.to_string()).collect(),
                            schools: schools
                                .iter()
                                .map(|(t, s)| HeldSchool {
                                    tenant_id: t.to_string(),
                                    school_id: s.to_string(),
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
        })
    }

    const GATES: [Gate; 3] = [
        Gate::FinanceView,
        Gate::FinanceApprove,
        Gate::AssistedSubmit,
    ];

    #[test]
    fn sis_answer_parses_the_documented_contract() {
        let raw = r#"{"data":{"staffMemberId":"staff-1","cacheSeconds":60,"capabilities":{"billing.payment.read":{"tenants":["TENANT-001"],"schools":[{"tenantId":"TENANT-001","schoolId":"SCH-IISS"}]}}}}"#;
        let held = serde_json::from_str::<Envelope>(raw).unwrap().data;
        assert_eq!(held.staff_member_id, STAFF);
        assert!(held.tenant_wide(STAFF, PAYMENT_READ, T));
        assert_eq!(
            held.capabilities[PAYMENT_READ].schools,
            vec![HeldSchool {
                tenant_id: T.into(),
                school_id: "SCH-IISS".into()
            }]
        );
    }

    #[test]
    fn role_answer_wins_and_no_answer_never_opens() {
        for gate in GATES {
            assert!(decide(true, Some(STAFF), gate, T, None));
            assert!(decide(true, None, gate, T, Some(&Err(Unavailable::Failed))));
            assert!(!decide(false, Some(STAFF), gate, T, None));
            assert!(!decide(false, Some(STAFF), gate, T, Some(&held(&[]))));
            for failure in [Unavailable::Off, Unavailable::Failed] {
                assert!(!decide(false, Some(STAFF), gate, T, Some(&Err(failure))));
            }
        }
    }

    #[test]
    fn each_held_capability_opens_only_its_gates() {
        let cases: [(&str, &[Gate]); 4] = [
            (PAYMENT_READ, &[Gate::FinanceView]),
            (PAYMENT_MANAGE, &[Gate::FinanceView, Gate::FinanceApprove]),
            (UPLOAD_ON_BEHALF, &[Gate::AssistedSubmit]),
            (SCHOOL_INVOICE_MANAGE, &[]),
        ];
        for (code, opens) in cases {
            let answer = held(&[(code, &[T], &[])]);
            for gate in GATES {
                assert_eq!(
                    decide(false, Some(STAFF), gate, T, Some(&answer)),
                    opens.contains(&gate),
                    "{code} / {gate:?}"
                );
            }
        }
    }

    #[test]
    fn school_only_other_tenant_or_other_staff_do_not_open_tenant_gates() {
        let all = [PAYMENT_READ, PAYMENT_MANAGE, UPLOAD_ON_BEHALF];
        for code in all {
            let school_only = held(&[(code, &[], &[(T, "SCH-IISS")])]);
            let other_tenant = held(&[(code, &["TENANT-002"], &[])]);
            for gate in GATES {
                assert!(!decide(false, Some(STAFF), gate, T, Some(&school_only)));
                assert!(!decide(false, Some(STAFF), gate, T, Some(&other_tenant)));
            }
            let tenant_wide = held(&[(code, &[T], &[])]);
            for gate in GATES {
                assert!(!decide(false, Some("staff-2"), gate, T, Some(&tenant_wide)));
                assert!(!decide(false, None, gate, T, Some(&tenant_wide)));
                assert!(!decide(false, Some(""), gate, T, Some(&tenant_wide)));
            }
        }
    }

    #[test]
    fn school_reach_keeps_own_valid_holdings_only() {
        let answer = held(&[(
            SCHOOL_INVOICE_MANAGE,
            &[T, "bad|tenant"],
            &[(T, "SCH-IISS"), (T, "SCH-IISS"), (T, "bad|school")],
        )]);
        let reach = school_reach(&answer, STAFF, SCHOOL_INVOICE_MANAGE);
        assert_eq!(reach.tenants, vec![T.to_string()]);
        assert_eq!(reach.schools, vec![format!("{T}|SCH-IISS")]);
        assert!(reach.any());
        assert_eq!(
            school_reach(&answer, "staff-2", SCHOOL_INVOICE_MANAGE),
            SchoolReach::default()
        );
        assert_eq!(
            school_reach(&answer, STAFF, PAYMENT_MANAGE),
            SchoolReach::default()
        );
        assert!(!school_reach(&Err(Unavailable::Failed), STAFF, SCHOOL_INVOICE_MANAGE).any());
        assert!(!school_reach(&held(&[]), STAFF, SCHOOL_INVOICE_MANAGE).any());
    }

    #[tokio::test]
    async fn sis_client_forwards_the_bearer_and_fails_closed() {
        let sis = stub::Sis::start().await;
        sis.holds(STAFF, &[(PAYMENT_READ, &[T], &[(T, "SCH-IISS")])]);
        let answer = held_from(&sis.origin, "token-1", &[PAYMENT_READ, PAYMENT_MANAGE]).await;
        assert!(decide(
            false,
            Some(STAFF),
            Gate::FinanceView,
            T,
            Some(&answer)
        ));
        assert_eq!(
            sis.calls(),
            vec![(
                "Bearer token-1".to_string(),
                format!("{PAYMENT_READ},{PAYMENT_MANAGE}")
            )]
        );
        for (status, body) in [
            (401, serde_json::json!({"error":{"code":"UNAUTHORIZED"}})),
            (400, serde_json::json!({"error":{"code":"INVALID_INPUT"}})),
            (503, serde_json::json!({})),
            (200, serde_json::json!({"data":{"unexpected":true}})),
            (
                200,
                serde_json::json!({"data":{"staffMemberId":STAFF,"capabilities":{"x":"y"}}}),
            ),
        ] {
            sis.reply(status, body);
            assert_eq!(
                held_from(&sis.origin, "token-1", &[PAYMENT_READ])
                    .await
                    .unwrap_err(),
                Unavailable::Failed,
                "{status}"
            );
        }
        // Nothing listens here any more once bound and dropped.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        assert_eq!(
            held_from(&closed, "token-1", &[PAYMENT_READ])
                .await
                .unwrap_err(),
            Unavailable::Failed
        );
    }
}
