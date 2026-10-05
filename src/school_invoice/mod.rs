//! Manual school billing. Separate invoice, evidence, audit and settlement
//! labels; no admissions transitions, notifications, scheduling or providers.
mod auth;
mod documents;
#[cfg(test)]
mod http_acceptance;
pub mod model;
mod parent_authority;
mod payee;
mod proof;
pub mod repository;
#[cfg(test)]
mod tests;
use crate::AppState;
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
type Failure = (StatusCode, Json<Value>);
fn failure(s: StatusCode) -> Failure {
    (
        s,
        Json(
            json!({"error":{"code":match s{StatusCode::UNAUTHORIZED=>"UNAUTHORIZED",StatusCode::FORBIDDEN=>"BILLING_ACCESS_DENIED",StatusCode::BAD_REQUEST=>"INVALID_INPUT",StatusCode::CONFLICT=>"REVISION_CONFLICT",_=>"BILLING_UNAVAILABLE"}}}),
        ),
    )
}
fn owner_error(e: repository::Error) -> Failure {
    failure(match e {
        repository::Error::Expired => StatusCode::UNAUTHORIZED,
        repository::Error::Denied => StatusCode::FORBIDDEN,
        repository::Error::Conflict => StatusCode::CONFLICT,
        repository::Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
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
fn path(id: &str) -> Result<(), Failure> {
    if model::identifier(id) && id.starts_with("SINV-") {
        Ok(())
    } else {
        Err(failure(StatusCode::BAD_REQUEST))
    }
}
async fn issue(
    State(s): State<AppState>,
    h: HeaderMap,
    input: Result<Json<model::Issue>, JsonRejection>,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let input = input.map_err(|_| failure(StatusCode::BAD_REQUEST))?.0;
    if !input.valid() {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    Ok(data(
        repository::issue(graph(&s)?, &a, &input)
            .await
            .map_err(owner_error)?,
    ))
}
async fn parent_list(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::parent(&h, &s.jwt_secret)?;
    Ok(data(
        repository::list_parent(graph(&s)?, &a)
            .await
            .map_err(owner_error)?,
    ))
}
async fn finance_list(State(s): State<AppState>, h: HeaderMap) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    Ok(data(
        repository::list_finance(graph(&s)?, &a)
            .await
            .map_err(owner_error)?,
    ))
}
async fn parent_detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    path(&id)?;
    let a = auth::parent(&h, &s.jwt_secret)?;
    Ok(data(
        repository::parent(graph(&s)?, &a, &id)
            .await
            .map_err(owner_error)?,
    ))
}
async fn finance_detail(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    path(&id)?;
    let a = auth::staff(&h, &s.jwt_secret)?;
    let g = graph(&s)?;
    let inv = repository::finance(g, &a, &id).await.map_err(owner_error)?;
    let proofs = repository::proofs(g, &id).await.map_err(owner_error)?;
    repository::finance(g, &a, &id).await.map_err(owner_error)?;
    Ok(data(json!({"invoice":inv,"proofs":proofs})))
}
async fn review(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    input: Result<Json<model::Review>, JsonRejection>,
) -> Result<Response, Failure> {
    path(&id)?;
    let a = auth::staff(&h, &s.jwt_secret)?;
    let i = input.map_err(|_| failure(StatusCode::BAD_REQUEST))?.0;
    if !i.valid() {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    Ok(data(
        repository::review(graph(&s)?, &a, &id, &i)
            .await
            .map_err(owner_error)?,
    ))
}
async fn pdf(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, kind)): Path<(String, String)>,
) -> Result<Response, Failure> {
    path(&id)?;
    if !matches!(kind.as_str(), "invoice.pdf" | "receipt.pdf") {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    let a = auth::parent(&h, &s.jwt_secret)?;
    let inv = repository::parent(graph(&s)?, &a, &id)
        .await
        .map_err(owner_error)?;
    let bytes = documents::render(&inv, kind == "receipt.pdf")
        .map_err(|_| failure(StatusCode::CONFLICT))?;
    if !auth::live(a.expires) {
        return Err(failure(StatusCode::UNAUTHORIZED));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf"),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/payments/school-invoices", get(parent_list))
        .route("/api/v1/payments/school-invoices/:id", get(parent_detail))
        .route(
            "/api/v1/payments/school-invoices/:id/proofs",
            post(proof::upload).layer(DefaultBodyLimit::max(10 * 1024 * 1024 + 8192)),
        )
        .route(
            "/api/v1/payments/school-invoices/:id/proofs/:proof/download",
            get(proof::download),
        )
        .route("/api/v1/payments/school-invoices/:id/:kind", get(pdf))
        .route(
            "/api/v1/payments/admin/school-invoices/configuration",
            get(school_configuration),
        )
        .route(
            "/api/v1/payments/admin/school-invoices/payee",
            post(configure_payee).layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/v1/payments/admin/school-invoices",
            get(finance_list)
                .post(issue)
                .layer(DefaultBodyLimit::max(16384)),
        )
        .route(
            "/api/v1/payments/admin/school-invoices/:id",
            get(finance_detail),
        )
        .route(
            "/api/v1/payments/admin/school-invoices/:id/review",
            post(review).layer(DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/v1/payments/admin/school-invoices/:id/proofs/:proof/download",
            get(proof::finance_download),
        )
}

async fn configure_payee(
    State(s): State<AppState>,
    h: HeaderMap,
    input: Result<Json<payee::Configure>, JsonRejection>,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    let input = input.map_err(|_| failure(StatusCode::BAD_REQUEST))?.0;
    if !input.valid() {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    Ok(data(
        payee::configure(graph(&s)?, &a, &input)
            .await
            .map_err(owner_error)?,
    ))
}

async fn school_configuration(
    State(s): State<AppState>,
    h: HeaderMap,
) -> Result<Response, Failure> {
    let a = auth::staff(&h, &s.jwt_secret)?;
    Ok(data(
        payee::list(graph(&s)?, &a).await.map_err(owner_error)?,
    ))
}
