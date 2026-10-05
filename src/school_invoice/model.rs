use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub fn identifier(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
pub fn text(v: &str, max: usize) -> bool {
    !v.trim().is_empty() && v.len() <= max && !v.chars().any(char::is_control)
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Issue {
    pub idempotency_key: String,
    pub student_id: String,
    pub payer_user_id: String,
    pub period: String,
    pub description: String,
    /// IDR major units, matching the existing Payment owner.
    pub amount: i64,
    pub currency: String,
    pub due_date: String,
    pub bank_account_id: String,
}
impl Issue {
    pub fn valid(&self) -> bool {
        [
            &self.idempotency_key,
            &self.student_id,
            &self.payer_user_id,
            &self.bank_account_id,
        ]
        .iter()
        .all(|v| identifier(v))
            && text(&self.period, 64)
            && text(&self.description, 256)
            && self.amount > 0
            && self.amount <= 1_000_000_000_000
            && self.currency == "IDR"
            && NaiveDate::parse_from_str(&self.due_date, "%Y-%m-%d")
                .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == self.due_date)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub contract_version: u32,
    pub id: String,
    pub tenant_id: String,
    pub school_id: String,
    pub school_code: String,
    pub payee_name: String,
    pub student_id: String,
    pub enrolled_student_id: String,
    pub student_name: String,
    pub payer_user_id: String,
    pub payer_name: String,
    pub period: String,
    pub description: String,
    pub amount: i64,
    pub currency: String,
    pub due_date: String,
    pub bank: crate::repositories::payment_settings_repository::ManualBankAccount,
    pub payee_version: i64,
    pub issued_at: String,
}
impl Snapshot {
    /// The recorded command binds this immutable payer/child/scope. Enrollment
    /// exit never changes it, and malformed/imported records cannot acquire
    /// financial access merely by copying a payer selector.
    pub fn matches_issuance(&self, key: &str, request_hash: &str) -> bool {
        use sha2::{Digest, Sha256};
        let prefix = format!("{}|{}|", self.tenant_id, self.school_id);
        let Some(idempotency_key) = key.strip_prefix(&prefix) else {
            return false;
        };
        let command = Issue {
            idempotency_key: idempotency_key.into(),
            student_id: self.student_id.clone(),
            payer_user_id: self.payer_user_id.clone(),
            period: self.period.clone(),
            description: self.description.clone(),
            amount: self.amount,
            currency: self.currency.clone(),
            due_date: self.due_date.clone(),
            bank_account_id: self.bank.id.clone(),
        };
        self.contract_version == 1
            && self.id.starts_with("SINV-")
            && [
                &self.id,
                &self.tenant_id,
                &self.school_id,
                &self.enrolled_student_id,
            ]
            .iter()
            .all(|v| identifier(v))
            && command.valid()
            && serde_json::to_vec(&command)
                .is_ok_and(|raw| hex::encode(Sha256::digest(raw)) == request_hash)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invoice {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    pub kind: String,
    pub status: String,
    pub version: i64,
    pub paid_at: Option<String>,
    pub receipt_ref: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Review {
    pub version: i64,
    pub proof_id: String,
    pub decision: String,
    pub note: String,
}
impl Review {
    pub fn valid(&self) -> bool {
        self.version >= 1
            && identifier(&self.proof_id)
            && matches!(self.decision.as_str(), "approve" | "reject")
            && text(&self.note, 512)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proof {
    pub id: String,
    pub invoice_id: String,
    pub amount_submitted: i64,
    pub paid_at: String,
    pub payer_name: String,
    pub payer_bank: String,
    pub reference_number: String,
    pub mime_type: String,
    pub file_name: String,
    pub size_bytes: i64,
    pub document_hash: String,
    pub uploaded_at: String,
}
impl Proof {
    pub fn valid(&self) -> bool {
        self.amount_submitted > 0
            && (chrono::DateTime::parse_from_rfc3339(&self.paid_at).is_ok()
                || NaiveDate::parse_from_str(&self.paid_at, "%Y-%m-%d")
                    .is_ok_and(|d| d.format("%Y-%m-%d").to_string() == self.paid_at))
            && text(&self.payer_name, 128)
            && text(&self.payer_bank, 128)
            && text(&self.reference_number, 128)
            && text(&self.file_name, 128)
            && self.size_bytes > 0
            && self.size_bytes <= 10 * 1024 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn issue() -> Issue {
        Issue {
            idempotency_key: "key-one".into(),
            student_id: "child-one".into(),
            payer_user_id: "parent-one".into(),
            period: "October 2026".into(),
            description: "Tuition".into(),
            amount: 100,
            currency: "IDR".into(),
            due_date: "2026-10-10".into(),
            bank_account_id: "bank-one".into(),
        }
    }
    #[test]
    fn explicit_idr_money_and_valid_calendar_date_required() {
        let mut i = issue();
        assert!(i.valid());
        i.amount = 0;
        assert!(!i.valid());
        i.amount = 100;
        i.currency = "USD".into();
        assert!(!i.valid());
        i.currency = "IDR".into();
        i.due_date = "2026-02-30".into();
        assert!(!i.valid());
        i.due_date = "2026-10-10".into();
        i.period.clear();
        assert!(!i.valid());
    }
    #[test]
    fn unknown_scope_and_roles_rejected() {
        let mut i = serde_json::to_value(issue()).unwrap();
        i["schoolId"] = serde_json::json!("foreign");
        assert!(serde_json::from_value::<Issue>(i).is_err());
    }
    #[test]
    fn issuance_hash_binds_original_family_student_amount_and_scope() {
        use sha2::{Digest, Sha256};
        let command = issue();
        let hash = hex::encode(Sha256::digest(serde_json::to_vec(&command).unwrap()));
        let raw = serde_json::json!({"contractVersion":1,"id":"SINV-original","tenantId":"tenant-one","schoolId":"school-one","schoolCode":"IISS","payeeName":"School","studentId":command.student_id,"enrolledStudentId":"permanent-child-one","studentName":"Child","payerUserId":command.payer_user_id,"payerName":"Parent","period":command.period,"description":command.description,"amount":command.amount,"currency":command.currency,"dueDate":command.due_date,"bank":{"id":command.bank_account_id,"bankName":"Bank","accountName":"School","accountNumber":"0000","instructions":"Synthetic only","enabled":true},"payeeVersion":1,"issuedAt":"2026-10-04T00:00:00Z"});
        let snapshot: Snapshot = serde_json::from_value(raw.clone()).unwrap();
        let key = "tenant-one|school-one|key-one";
        assert!(snapshot.matches_issuance(key, &hash));
        for (field, value) in [
            ("studentId", serde_json::json!("unrelated-child")),
            ("payerUserId", serde_json::json!("foreign-parent")),
            ("amount", serde_json::json!(101)),
            ("enrolledStudentId", serde_json::json!("")),
            ("contractVersion", serde_json::json!(2)),
        ] {
            let mut changed = raw.clone();
            changed[field] = value;
            assert!(!serde_json::from_value::<Snapshot>(changed)
                .unwrap()
                .matches_issuance(key, &hash));
        }
        assert!(!snapshot.matches_issuance("foreign-tenant|school-one|key-one", &hash));
    }
}
