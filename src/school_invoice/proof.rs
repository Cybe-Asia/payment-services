use super::{auth, data, failure, graph, model::*, owner_error, path, repository, Failure};
use crate::AppState;
use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn allowed(mime: &str, b: &[u8]) -> bool {
    match mime {
        "application/pdf" => b.starts_with(b"%PDF-"),
        "image/png" => b.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => b.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP",
        _ => false,
    }
}
pub async fn upload(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Response, Failure> {
    path(&id)?;
    let a = auth::parent(&h, &s.jwt_secret)?;
    let g = graph(&s)?;
    let inv = repository::parent(g, &a, &id).await.map_err(owner_error)?;
    if !matches!(inv.status.as_str(), "pending" | "rejected") {
        return Err(failure(StatusCode::CONFLICT));
    }
    let minio = s
        .minio
        .as_ref()
        .ok_or_else(|| failure(StatusCode::SERVICE_UNAVAILABLE))?;
    let mut fields = BTreeMap::new();
    let mut file = None;
    while let Some(mut f) = multipart
        .next_field()
        .await
        .map_err(|_| failure(StatusCode::BAD_REQUEST))?
    {
        let name = f
            .name()
            .ok_or_else(|| failure(StatusCode::BAD_REQUEST))?
            .to_string();
        if name == "file" {
            if file.is_some() {
                return Err(failure(StatusCode::BAD_REQUEST));
            }
            let mime = f.content_type().unwrap_or("").to_string();
            let filename = f.file_name().unwrap_or("").to_string();
            let mut bytes = Vec::new();
            while let Some(chunk) = f
                .chunk()
                .await
                .map_err(|_| failure(StatusCode::BAD_REQUEST))?
            {
                if bytes.len() + chunk.len() > 10 * 1024 * 1024 {
                    return Err(failure(StatusCode::BAD_REQUEST));
                }
                bytes.extend_from_slice(&chunk);
            }
            if !allowed(&mime, &bytes) {
                return Err(failure(StatusCode::BAD_REQUEST));
            }
            file = Some((mime, filename, bytes));
        } else {
            if ![
                "amountSubmitted",
                "paidAt",
                "payerName",
                "payerBank",
                "referenceNumber",
            ]
            .contains(&name.as_str())
                || fields.contains_key(&name)
            {
                return Err(failure(StatusCode::BAD_REQUEST));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = f
                .chunk()
                .await
                .map_err(|_| failure(StatusCode::BAD_REQUEST))?
            {
                if bytes.len() + chunk.len() > 256 {
                    return Err(failure(StatusCode::BAD_REQUEST));
                }
                bytes.extend_from_slice(&chunk);
            }
            fields.insert(
                name,
                String::from_utf8(bytes).map_err(|_| failure(StatusCode::BAD_REQUEST))?,
            );
        }
    }
    let (mime, filename, bytes) = file.ok_or_else(|| failure(StatusCode::BAD_REQUEST))?;
    let field = |k: &str| {
        fields
            .get(k)
            .cloned()
            .ok_or_else(|| failure(StatusCode::BAD_REQUEST))
    };
    let p = Proof {
        id: format!("SPROOF-{}", uuid::Uuid::new_v4()),
        invoice_id: id.clone(),
        amount_submitted: field("amountSubmitted")?
            .parse()
            .map_err(|_| failure(StatusCode::BAD_REQUEST))?,
        paid_at: field("paidAt")?,
        payer_name: field("payerName")?,
        payer_bank: field("payerBank")?,
        reference_number: field("referenceNumber")?,
        mime_type: mime,
        file_name: filename,
        size_bytes: bytes.len() as i64,
        document_hash: hex::encode(Sha256::digest(&bytes)),
        uploaded_at: chrono::Utc::now().to_rfc3339(),
    };
    if !p.valid() {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    let object = format!("school-test/payments/{id}/{}", p.id);
    minio
        .put_encrypted_document(&object, bytes.into())
        .await
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE))?;
    match repository::record_proof(g, &a, &inv, &p, &object).await {
        Ok(inv) => Ok(data(inv)),
        Err(e) => {
            let _ = minio.delete_object(&object).await;
            Err(owner_error(e))
        }
    }
}
async fn bytes(s: &AppState, id: &str, proof: &str) -> Result<Response, Failure> {
    if !identifier(proof) {
        return Err(failure(StatusCode::BAD_REQUEST));
    }
    let (object, p) = repository::proof_object(graph(s)?, id, proof)
        .await
        .map_err(owner_error)?;
    let bytes = s
        .minio
        .as_ref()
        .ok_or_else(|| failure(StatusCode::SERVICE_UNAVAILABLE))?
        .get_decrypted_document(&object)
        .await
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE))?;
    if bytes.len() as i64 != p.size_bytes || hex::encode(Sha256::digest(&bytes)) != p.document_hash
    {
        return Err(failure(StatusCode::SERVICE_UNAVAILABLE));
    }
    Ok((
        [
            (header::CONTENT_TYPE, p.mime_type),
            (header::CONTENT_DISPOSITION, "attachment".into()),
            (header::CACHE_CONTROL, "no-store".into()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        bytes,
    )
        .into_response())
}
pub async fn download(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, proof)): Path<(String, String)>,
) -> Result<Response, Failure> {
    path(&id)?;
    let a = auth::parent(&h, &s.jwt_secret)?;
    repository::parent(graph(&s)?, &a, &id)
        .await
        .map_err(owner_error)?;
    let result = bytes(&s, &id, &proof).await?;
    repository::parent(graph(&s)?, &a, &id)
        .await
        .map_err(owner_error)?;
    Ok(result)
}
pub async fn finance_download(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, proof)): Path<(String, String)>,
) -> Result<Response, Failure> {
    path(&id)?;
    let a = auth::staff(&h, &s.jwt_secret)?;
    repository::finance(graph(&s)?, &a, &id)
        .await
        .map_err(owner_error)?;
    let result = bytes(&s, &id, &proof).await?;
    repository::finance(graph(&s)?, &a, &id)
        .await
        .map_err(owner_error)?;
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_type_requires_matching_signature() {
        assert!(allowed("application/pdf", b"%PDF-1.4\nfixture"));
        assert!(!allowed("application/pdf", b"<script>"));
        assert!(!allowed("text/html", b"%PDF-1.4"));
        assert!(allowed("image/png", b"\x89PNG\r\n\x1a\nbytes"));
    }
}
