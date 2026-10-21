//! Parent school billing: Advance Monthly Fee, Capital Levy, Overseas Program
//! and Student Credit on top of `SchoolInvoice` v1 (ADR 0014).
pub mod checkout;
pub mod model;
pub mod repository;
pub mod service;
#[cfg(test)]
mod tests;
pub mod worker;

use crate::{
    school_invoice::{auth, model::identifier, repository::Error},
    AppState,
};
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub type Failure = (StatusCode, Json<Value>);
pub fn coded(s: StatusCode, code: &str) -> Failure {
    (s, Json(json!({"error":{"code":code}})))
}
pub fn failure(s: StatusCode) -> Failure {
    coded(
        s,
        match s {
            StatusCode::UNAUTHORIZED => "UNAUTHORIZED",
            StatusCode::FORBIDDEN => "BILLING_ACCESS_DENIED",
            StatusCode::BAD_REQUEST => "INVALID_INPUT",
            StatusCode::CONFLICT => "REVISION_CONFLICT",
            _ => "BILLING_UNAVAILABLE",
        },
    )
}
pub fn owner_error(e: Error) -> Failure {
    failure(match e {
        Error::Expired => StatusCode::UNAUTHORIZED,
        Error::Denied => StatusCode::FORBIDDEN,
        Error::Conflict => StatusCode::CONFLICT,
        Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    })
}
fn graph(s: &AppState) -> Result<&neo4rs::Graph, Failure> {
    s.graph
        .as_deref()
        .ok_or_else(|| failure(StatusCode::SERVICE_UNAVAILABLE))
}
fn data(v: impl serde::Serialize) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"data":v})),
    )
        .into_response()
}
fn body<T>(input: Result<Json<T>, JsonRejection>) -> Result<T, Failure> {
    input
        .map(|j| j.0)
        .map_err(|_| failure(StatusCode::BAD_REQUEST))
}
fn ids(values: &[&str]) -> Result<(), Failure> {
    if values.iter().all(|v| identifier(v)) {
        Ok(())
    } else {
        Err(failure(StatusCode::BAD_REQUEST))
    }
}

// ------------------------------------------------------------------ parent

async fn children(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::parent(&h, &s.jwt_secret)?;
    let g = graph(&s)?;
    let mut views = vec![];
    for c in repository::children(g, &a).await.map_err(owner_error)? {
        views.push(service::summary(g, c).await.map_err(owner_error)?);
    }
    if !auth::live(a.expires) {
        return Err(failure(StatusCode::UNAUTHORIZED));
    }
    Ok(data(json!({
        "students": views,
        "onlinePayment": s.doku.configured() && std::env::var("SCHOOL_BILLING_RETURN_URL").is_ok(),
    })))
}

async fn child(
    s: &AppState,
    h: &HeaderMap,
    student: &str,
) -> Result<(auth::Parent, repository::Child), Failure> {
    ids(&[student])?;
    let a = auth::parent(h, &s.jwt_secret)?;
    let c = repository::family(graph(s)?, &a.subject, student)
        .await
        .map_err(owner_error)?;
    Ok((a, c))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Select {
    months: i32,
    version: i64,
}
async fn select(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(student): Path<String>,
    input: Result<Json<Select>, JsonRejection>,
) -> Result<Response, Failure> {
    let input = body(input)?;
    let (a, c) = child(&s, &h, &student).await?;
    let g = graph(&s)?;
    let plan = repository::plan_for(g, &c)
        .await
        .map_err(owner_error)?
        .ok_or_else(|| coded(StatusCode::CONFLICT, "NOT_CONFIGURED"))?;
    if !plan.input.options.iter().any(|o| o.months == input.months) || input.version < 0 {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    repository::select(g, &a, &c, input.months, input.version)
        .await
        .map_err(|e| match e {
            Error::Conflict => coded(StatusCode::CONFLICT, "PERIOD_LOCKED"),
            e => owner_error(e),
        })?;
    // The first period of a new choice is billed at once when it is due.
    if let Err(e) = service::issue_monthly(g, &c, model::today()).await {
        tracing::warn!(error = ?e, "monthly fee was not issued after selection");
    }
    Ok(data(service::summary(g, c).await.map_err(owner_error)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Amount {
    amount: i64,
    request_id: String,
}
async fn levy_deposit(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(student): Path<String>,
    input: Result<Json<Amount>, JsonRejection>,
) -> Result<Response, Failure> {
    let input = body(input)?;
    ids(&[&input.request_id])?;
    let (_, c) = child(&s, &h, &student).await?;
    let g = graph(&s)?;
    // Deposits go to the newest academic year billed through the Payment tab.
    let levy = repository::capital_levies_for(g, &c)
        .await
        .map_err(owner_error)?
        .into_iter()
        .next()
        .ok_or_else(|| coded(StatusCode::CONFLICT, "NOT_CONFIGURED"))?;
    Ok(data(
        service::deposit(g, &c, &levy, input.amount, &input.request_id).await?,
    ))
}

async fn program(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((student, id)): Path<(String, String)>,
) -> Result<Response, Failure> {
    ids(&[&id])?;
    let (_, c) = child(&s, &h, &student).await?;
    let g = graph(&s)?;
    let p = repository::program_for(g, &c, &id)
        .await
        .map_err(owner_error)?;
    let today = model::today();
    let early = model::early_bird_open(p.input.early_bird_until.as_deref(), today);
    let enrollment = repository::enrollments(g, &c)
        .await
        .map_err(owner_error)?
        .into_iter()
        .find(|e| e.program_id == p.id);
    Ok(data(json!({
        "id": p.id,
        "title": p.input.title,
        "summary": p.input.summary,
        "terms": p.input.terms,
        "termsVersion": p.terms_version,
        "participants": p.participants,
        "quota": p.input.quota,
        "status": p.input.status,
        "earlyBirdUntil": p.input.early_bird_until,
        "earlyBirdOpen": early,
        "scheme": if early { "early_bird" } else { "regular" },
        "installments": if early { &p.input.early_bird_installments } else { &p.input.installments },
        "discount": if early { p.input.early_bird_discount } else { 0 },
        "enrollment": enrollment,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Enroll {
    terms_version: String,
}
async fn enroll(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((student, id)): Path<(String, String)>,
    input: Result<Json<Enroll>, JsonRejection>,
) -> Result<Response, Failure> {
    let input = body(input)?;
    ids(&[&id, &input.terms_version])?;
    let (a, c) = child(&s, &h, &student).await?;
    let g = graph(&s)?;
    let p = repository::program_for(g, &c, &id)
        .await
        .map_err(owner_error)?;
    if p.input.status != "open" {
        return Err(failure(StatusCode::FORBIDDEN));
    }
    if p.terms_version != input.terms_version {
        return Err(coded(StatusCode::CONFLICT, "TERMS_CHANGED"));
    }
    if p.participants >= p.input.quota {
        return Err(coded(StatusCode::CONFLICT, "QUOTA_FULL"));
    }
    let today = model::today();
    let enrollment = repository::enroll(g, &a, &c, &p, &input.terms_version, today)
        .await
        .map_err(|e| match e {
            // Quota, terms or a second enrollment changed under the parent.
            Error::Conflict => coded(StatusCode::CONFLICT, "REVISION_CONFLICT"),
            e => owner_error(e),
        })?;
    if let Err(e) = service::issue_installment(g, &c, &enrollment, today).await {
        tracing::warn!(error = ?e, "overseas installment was not issued after enrollment");
    }
    Ok(data(service::summary(g, c).await.map_err(owner_error)?))
}
async fn overseas_deposit(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((student, id)): Path<(String, String)>,
    input: Result<Json<Amount>, JsonRejection>,
) -> Result<Response, Failure> {
    let input = body(input)?;
    ids(&[&id, &input.request_id])?;
    let (_, c) = child(&s, &h, &student).await?;
    let g = graph(&s)?;
    let enrollment = repository::enrollments(g, &c)
        .await
        .map_err(owner_error)?
        .into_iter()
        .find(|e| e.program_id == id)
        .ok_or_else(|| failure(StatusCode::FORBIDDEN))?;
    Ok(data(
        service::deposit(g, &c, &enrollment, input.amount, &input.request_id).await?,
    ))
}
async fn top_up(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(student): Path<String>,
    input: Result<Json<Amount>, JsonRejection>,
) -> Result<Response, Failure> {
    let input = body(input)?;
    ids(&[&input.request_id])?;
    let (_, c) = child(&s, &h, &student).await?;
    Ok(data(
        service::top_up(graph(&s)?, &c, input.amount, &input.request_id).await?,
    ))
}

fn invoice_path(id: &str) -> Result<(), Failure> {
    if identifier(id) && id.starts_with(checkout::PREFIX) {
        Ok(())
    } else {
        Err(failure(StatusCode::BAD_REQUEST))
    }
}
async fn start_checkout(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    invoice_path(&id)?;
    let a = auth::parent(&h, &s.jwt_secret)?;
    Ok(data(checkout::start(&s, graph(&s)?, &a, &id).await?))
}
async fn refresh_checkout(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    invoice_path(&id)?;
    let a = auth::parent(&h, &s.jwt_secret)?;
    Ok(data(checkout::refresh(&s, graph(&s)?, &a, &id).await?))
}

// ------------------------------------------------------------------- admin

async fn plans(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    Ok(data(
        repository::plans(graph(&s)?, &a)
            .await
            .map_err(owner_error)?,
    ))
}
async fn save_plan(
    State(s): State<AppState>,
    h: HeaderMap,
    input: Result<Json<model::FeePlanInput>, JsonRejection>,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let input = body(input)?;
    if !input.valid() {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    Ok(data(
        repository::save_plan(graph(&s)?, &a, &input)
            .await
            .map_err(owner_error)?,
    ))
}
async fn staff_students(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    Ok(data(
        repository::staff_students(graph(&s)?, &a)
            .await
            .map_err(owner_error)?,
    ))
}
async fn levies(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let g = graph(&s)?;
    let mut out = vec![];
    for levy in repository::capital_levies(g, &a)
        .await
        .map_err(owner_error)?
    {
        let all = repository::bills(g, &levy.payer_user_id, &levy.enrolled_student_id)
            .await
            .map_err(owner_error)?;
        out.push(service::ScheduleView::of(levy, &all));
    }
    Ok(data(out))
}
async fn levy_plans(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    Ok(data(
        repository::levy_plans(graph(&s)?, &a)
            .await
            .map_err(owner_error)?,
    ))
}
async fn save_levy_plan(
    State(s): State<AppState>,
    h: HeaderMap,
    input: Result<Json<model::CapitalLevyPlanInput>, JsonRejection>,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let input = body(input)?;
    if !input.valid() {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    let saved = repository::save_levy_plan(graph(&s)?, &a, &input)
        .await
        .map_err(owner_error)?;
    // The school's children get this year's levy now rather than at the next
    // worker run; the request does not wait for it.
    if let Some(g) = s.graph.clone() {
        let (tenant, school) = (input.tenant_id.clone(), input.school_id.clone());
        tokio::spawn(async move {
            if let Err(e) = worker::run_school(&g, &tenant, &school).await {
                tracing::warn!(error = ?e, "capital levy plan was not applied at once");
            }
        });
    }
    Ok(data(saved))
}
async fn programs(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    Ok(data(
        repository::programs(graph(&s)?, &a)
            .await
            .map_err(owner_error)?,
    ))
}
async fn create_program(
    State(s): State<AppState>,
    h: HeaderMap,
    input: Result<Json<model::OverseasProgramInput>, JsonRejection>,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let input = body(input)?;
    if !input.valid() || input.version != 0 {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    Ok(data(
        repository::save_program(graph(&s)?, &a, None, &input)
            .await
            .map_err(owner_error)?,
    ))
}
async fn update_program(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    input: Result<Json<model::OverseasProgramInput>, JsonRejection>,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let input = body(input)?;
    if !input.valid() || input.version < 1 {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    Ok(data(
        repository::save_program(graph(&s)?, &a, Some(&id), &input)
            .await
            .map_err(owner_error)?,
    ))
}

pub fn router() -> Router<AppState> {
    const P: &str = "/api/v1/payments/school-billing";
    const A: &str = "/api/v1/payments/admin/school-billing";
    Router::new()
        .route(&format!("{P}/students"), get(children))
        .route(&format!("{P}/students/:student/monthly-fee"), put(select))
        .route(
            &format!("{P}/students/:student/capital-levy/deposits"),
            post(levy_deposit),
        )
        .route(
            &format!("{P}/students/:student/overseas/:program"),
            get(program),
        )
        .route(
            &format!("{P}/students/:student/overseas/:program/enrollments"),
            post(enroll),
        )
        .route(
            &format!("{P}/students/:student/overseas/:program/deposits"),
            post(overseas_deposit),
        )
        .route(
            &format!("{P}/students/:student/credit/top-ups"),
            post(top_up),
        )
        .route(
            "/api/v1/payments/school-invoices/:id/checkout",
            post(start_checkout),
        )
        .route(
            "/api/v1/payments/school-invoices/:id/checkout/status",
            post(refresh_checkout),
        )
        .route(&format!("{A}/fee-plans"), get(plans).put(save_plan))
        .route(&format!("{A}/students"), get(staff_students))
        .route(&format!("{A}/capital-levies"), get(levies))
        .route(
            &format!("{A}/capital-levy-plans"),
            get(levy_plans).put(save_levy_plan),
        )
        .route(
            &format!("{A}/overseas-programs"),
            get(programs).post(create_program),
        )
        .route(&format!("{A}/overseas-programs/:id"), put(update_program))
        .layer(DefaultBodyLimit::max(64 * 1024))
}
