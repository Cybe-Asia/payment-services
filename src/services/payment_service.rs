use std::sync::Arc;

use chrono::{Duration, Utc};
use neo4rs::{Graph, Query};
use tracing::{info, warn};
use uuid::Uuid;

use crate::clients::xendit::{CreateInvoiceRequest as XenditInvoiceReq, XenditClient};
use crate::models::payment::Payment;
use crate::models::payment_proof::PaymentProof;
use crate::repositories::payment_repository::{
    CreateProofInput, ManualBankDetails, PaymentReviewDetail, PaymentReviewRow,
};
use crate::repositories::payment_settings_repository::{
    ManualBankAccount, PaymentSettings, PaymentSettingsSeed, UpdatePaymentSettings,
};
use crate::repositories::{
    fee_obligation_repository, fee_structure_repository, payment_repository,
    payment_settings_repository,
    promotion_repository::{self, PromotionRuleSnapshot},
};

pub use payment_repository::PaymentReviewFilters;

#[derive(Debug)]
pub struct CreateInvoiceOutcome {
    pub payment_id: String,
    pub hosted_invoice_url: String,
    pub amount: i64,
    pub gross_amount: i64,
    pub discount_amount: i64,
    pub net_amount: i64,
    pub promotion_code: Option<String>,
    pub promotion_rule_id: Option<String>,
    pub line_items: Vec<PaymentLineItem>,
    pub currency: String,
    pub expires_at: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ManualPaymentOutcome {
    pub payment: Payment,
    pub settings: PaymentSettings,
}

#[derive(Debug, serde::Serialize)]
pub struct PaymentReviewList {
    pub rows: Vec<PaymentReviewRow>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct PaymentNotificationContext {
    pub parent_name: String,
    pub email: String,
    pub whatsapp: String,
    pub school: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationChannelPreference {
    event: String,
    channels: Vec<String>,
}

pub const NOTIFICATION_CHANNEL_EMAIL: &str = "email";
pub const NOTIFICATION_CHANNEL_WHATSAPP: &str = "whatsapp";

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewManualPaymentRequest {
    pub decision: String,
    pub verified_amount: Option<i64>,
    pub note: Option<String>,
}

pub struct PaymentContext<'a> {
    pub graph: Arc<Graph>,
    pub xendit: &'a XenditClient,
    pub tenant_id: &'a str,
    pub default_due_hours: i64,
    pub settings_seed: PaymentSettingsSeed,
}

const STATIC_QRIS_ACCOUNT_ID: &str = "__qris";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentLineItem {
    pub label: String,
    pub amount: i64,
}

#[derive(Debug, Clone)]
struct PaymentCalculation {
    gross_amount: i64,
    discount_amount: i64,
    net_amount: i64,
    promotion_code: Option<String>,
    promotion_rule_id: Option<String>,
    promotion_snapshot_json: Option<String>,
    line_items_json: String,
    line_items: Vec<PaymentLineItem>,
}

async fn calculate_payment(
    graph: &Graph,
    lead_id: &str,
    school_code: &str,
    payment_type: &str,
    unit_amount: i64,
    _currency: &str,
    student_count: i64,
) -> Result<PaymentCalculation, String> {
    let gross_amount = (unit_amount * student_count).max(0);
    // One payment, one promo: rank every valid candidate by the rupiah it
    // actually saves on THIS gross and keep the winner. Ties prefer the
    // promo a human attached (most specific) over the global ladder, and
    // the ladder over reference-code perks.
    let candidates = promotion_repository::find_candidates_for_lead(graph, lead_id, payment_type)
        .await
        .map_err(|e| format!("promotion lookup failed: {e}"))?;
    let rule = candidates
        .into_iter()
        .map(|rule| (calculate_discount_amount(gross_amount, &rule), rule))
        .filter(|(discount, _)| *discount > 0)
        .max_by_key(|(discount, rule)| {
            let priority = match rule.source.as_str() {
                "lead_promotion_code" => 2i64,
                "global_ladder" => 1,
                _ => 0,
            };
            (*discount, priority)
        })
        .map(|(_, rule)| rule);
    let discount_amount = rule
        .as_ref()
        .map(|rule| calculate_discount_amount(gross_amount, rule))
        .unwrap_or(0);
    let net_amount = (gross_amount - discount_amount).max(0);

    let mut line_items = vec![PaymentLineItem {
        label: format!("{} — {}", pretty_payment_type(payment_type), school_code),
        amount: gross_amount,
    }];
    if let Some(rule) = &rule {
        if discount_amount > 0 {
            let label_prefix = match rule.source.as_str() {
                "lead_promotion_code" => "Promotion discount",
                "global_ladder" => "Early-bird discount",
                _ => "Reference code discount",
            };
            line_items.push(PaymentLineItem {
                label: format!("{} ({})", label_prefix, rule.promotion_code),
                amount: -discount_amount,
            });
        }
    }
    let line_items_json =
        serde_json::to_string(&line_items).map_err(|e| format!("line item encode failed: {e}"))?;
    let promotion_snapshot_json = rule.as_ref().map(|rule| {
        serde_json::json!({
            "promotionCode": rule.promotion_code,
            "promotionRuleId": rule.promotion_rule_id,
            "promotionSource": rule.source,
            "discountType": rule.discount_type,
            "discountValue": rule.discount_value,
            "maxDiscountAmount": rule.max_discount_amount,
            "minNetAmount": rule.min_net_amount,
        })
        .to_string()
    });

    Ok(PaymentCalculation {
        gross_amount,
        discount_amount,
        net_amount,
        promotion_code: rule.as_ref().map(|rule| rule.promotion_code.clone()),
        promotion_rule_id: rule.as_ref().map(|rule| rule.promotion_rule_id.clone()),
        promotion_snapshot_json,
        line_items_json,
        line_items,
    })
}

fn calculate_discount_amount(gross_amount: i64, rule: &PromotionRuleSnapshot) -> i64 {
    if gross_amount <= 0 {
        return 0;
    }
    let raw_discount = match rule.discount_type.as_str() {
        "fixed_amount" => rule.discount_value,
        "percent" => gross_amount
            .saturating_mul(rule.discount_value)
            .saturating_div(100),
        _ => 0,
    }
    .max(0);
    let capped = rule
        .max_discount_amount
        .map(|cap| raw_discount.min(cap.max(0)))
        .unwrap_or(raw_discount);
    let min_net = rule.min_net_amount.unwrap_or(0).max(0);
    let max_allowed_discount = (gross_amount - min_net).max(0);
    capped.min(max_allowed_discount)
}

pub async fn create_invoice(
    ctx: PaymentContext<'_>,
    admission_id: &str,
    payment_type: &str,
) -> Result<CreateInvoiceOutcome, String> {
    let settings = get_payment_settings(&ctx.graph, &ctx.settings_seed).await?;
    if !settings.xendit_enabled {
        return Err("xendit payment method is disabled".to_string());
    }

    // 1) Look up the Lead (parent contact info + school selection).
    let lead = fetch_lead(&ctx.graph, admission_id)
        .await?
        .ok_or_else(|| "Lead not found".to_string())?;

    // 2) Resolve school_id from the lead's target_school_preference (code).
    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        &ctx.graph,
        ctx.tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| {
        format!(
            "school not found for code {}",
            lead.target_school_preference
        )
    })?;

    // 3) Look up active FeeStructure to get the *per-student* amount.
    let fs =
        fee_structure_repository::find_active(&ctx.graph, ctx.tenant_id, &school_id, payment_type)
            .await
            .map_err(|e| format!("fee structure lookup failed: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no active FeeStructure for {} / {}",
                    school_id, payment_type
                )
            })?;

    // 3b) Count students.
    //     application_fee: charged per-child for the *whole* application
    //                      (2 kids in one Lead → 2 × fee).
    //     enrolment_fee:   per-child (one offer → one invoice → one kid).
    //                      admissionId here is the Student id itself.
    //     Detect by prefix — STU- means we treat it as a single-student
    //     invoice; anything else is the Lead-wide application fee.
    let is_student_scoped = admission_id.starts_with("STU-") || payment_type == "enrolment_fee";
    let student_count = if is_student_scoped {
        1_i64
    } else {
        count_students_for_lead(&ctx.graph, admission_id).await?
    };
    if student_count == 0 {
        return Err(
            "no students registered for this application — add at least one student before paying"
                .to_string(),
        );
    }
    let calculation = calculate_payment(
        &ctx.graph,
        &lead.lead_id,
        &fs.school_code,
        payment_type,
        fs.amount,
        &fs.currency,
        student_count,
    )
    .await?;

    // 4) Create (or reuse pending) FeeObligation with the *scaled* total.
    let due_at = Utc::now() + Duration::hours(ctx.default_due_hours);
    let due_iso = due_at.to_rfc3339();
    let obligation_id = format!("FEEOBL-{}", Uuid::new_v4());
    let obligation = fee_obligation_repository::upsert_for_lead(
        &ctx.graph,
        fee_obligation_repository::FeeObligationUpsert {
            tenant_id: ctx.tenant_id,
            lead_id: &lead.lead_id,
            obligation_type: payment_type,
            amount_due: calculation.net_amount,
            currency: &fs.currency,
            due_iso: &due_iso,
            new_id: &obligation_id,
            gross_amount: calculation.gross_amount,
            discount_amount: calculation.discount_amount,
            promotion_code: calculation.promotion_code.as_deref(),
            promotion_rule_id: calculation.promotion_rule_id.as_deref(),
            promotion_snapshot_json: calculation.promotion_snapshot_json.as_deref(),
            line_items_json: &calculation.line_items_json,
        },
    )
    .await
    .map_err(|e| format!("fee obligation upsert failed: {e}"))?;

    let payment_id = format!("PAY-{}", Uuid::new_v4());

    // 5) Create Xendit invoice with the scaled total.
    let description = format!(
        "{} — {} × {} student{}",
        pretty_payment_type(payment_type),
        fs.school_code,
        student_count,
        if student_count == 1 { "" } else { "s" },
    );
    let xendit_req = XenditInvoiceReq {
        external_id: &payment_id,
        amount: calculation.net_amount,
        currency: &fs.currency,
        description: &description,
        payer_email: &lead.email,
        customer_name: &lead.parent_name,
        customer_phone: &lead.whatsapp,
        invoice_duration_seconds: ctx.default_due_hours * 3600,
    };
    let invoice = ctx.xendit.create_invoice(&xendit_req).await?;

    // 6) Persist Payment node linked to Lead and FeeObligation.
    payment_repository::create_pending(
        &ctx.graph,
        &payment_id,
        ctx.tenant_id,
        payment_type,
        calculation.net_amount,
        &fs.currency,
        &invoice.id,
        &invoice.id,
        &invoice.invoice_url,
        &invoice
            .expiry_date
            .clone()
            .unwrap_or_else(|| due_at.to_rfc3339()),
        &obligation.fee_obligation_id,
        &lead.lead_id,
        calculation.gross_amount,
        calculation.discount_amount,
        calculation.promotion_code.as_deref(),
        calculation.promotion_rule_id.as_deref(),
        calculation.promotion_snapshot_json.as_deref(),
        &calculation.line_items_json,
    )
    .await
    .map_err(|e| format!("payment persist failed: {e}"))?;

    // 7) Advance the Application lifecycle `submitted → payment_pending`
    //    (best-effort — the Application node is owned by admission-service
    //    but we share the neo4j, so a direct cypher is simpler than an
    //    HTTP hop. If the Application doesn't exist yet, the UPDATE just
    //    matches zero rows.)
    set_application_status_for_lead(&ctx.graph, &lead.lead_id, "submitted", "payment_pending")
        .await;

    info!(
        payment_id=%payment_id,
        admission_id=%admission_id,
        unit_amount=fs.amount,
        student_count=student_count,
        gross_amount=calculation.gross_amount,
        discount_amount=calculation.discount_amount,
        net_amount=calculation.net_amount,
        currency=%fs.currency,
        "created pending payment + xendit invoice"
    );

    Ok(CreateInvoiceOutcome {
        payment_id,
        hosted_invoice_url: invoice.invoice_url,
        amount: calculation.net_amount,
        gross_amount: calculation.gross_amount,
        discount_amount: calculation.discount_amount,
        net_amount: calculation.net_amount,
        promotion_code: calculation.promotion_code,
        promotion_rule_id: calculation.promotion_rule_id,
        line_items: calculation.line_items,
        currency: fs.currency,
        expires_at: invoice.expiry_date.unwrap_or_else(|| due_at.to_rfc3339()),
    })
}

pub async fn create_manual_payment(
    ctx: PaymentContext<'_>,
    admission_id: &str,
    payment_type: &str,
    manual_bank_account_id: Option<&str>,
) -> Result<ManualPaymentOutcome, String> {
    let settings = get_payment_settings(&ctx.graph, &ctx.settings_seed).await?;
    if !settings.manual_transfer_enabled && !settings.qris_enabled {
        return Err("proof-based payment methods are disabled".to_string());
    }

    let lead = fetch_lead(&ctx.graph, admission_id)
        .await?
        .ok_or_else(|| "Lead not found".to_string())?;

    if let Some(existing) =
        payment_repository::find_active_manual_for_lead(&ctx.graph, &lead.lead_id, payment_type)
            .await
            .map_err(|e| format!("manual payment lookup failed: {e}"))?
    {
        if let Some(selected_id) = manual_bank_account_id {
            if can_update_manual_destination(&existing) {
                let bank = resolve_manual_bank_details(&settings, Some(selected_id))?;
                if should_update_manual_destination(&existing, &bank) {
                    payment_repository::update_manual_bank_details(
                        &ctx.graph,
                        &existing.payment_id,
                        &bank,
                    )
                    .await
                    .map_err(|e| format!("manual payment bank update failed: {e}"))?;

                    let updated = payment_repository::find_by_id(&ctx.graph, &existing.payment_id)
                        .await
                        .map_err(|e| format!("manual payment fetch failed: {e}"))?
                        .ok_or_else(|| "manual payment updated but not found".to_string())?;

                    return Ok(ManualPaymentOutcome {
                        payment: updated,
                        settings,
                    });
                }
            }
        }

        return Ok(ManualPaymentOutcome {
            payment: existing,
            settings,
        });
    }

    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        &ctx.graph,
        ctx.tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| {
        format!(
            "school not found for code {}",
            lead.target_school_preference
        )
    })?;

    let fs =
        fee_structure_repository::find_active(&ctx.graph, ctx.tenant_id, &school_id, payment_type)
            .await
            .map_err(|e| format!("fee structure lookup failed: {e}"))?
            .ok_or_else(|| {
                format!(
                    "no active FeeStructure for {} / {}",
                    school_id, payment_type
                )
            })?;

    let is_student_scoped = admission_id.starts_with("STU-") || payment_type == "enrolment_fee";
    let student_count = if is_student_scoped {
        1_i64
    } else {
        count_students_for_lead(&ctx.graph, &lead.lead_id).await?
    };
    if student_count == 0 {
        return Err(
            "no students registered for this application — add at least one student before paying"
                .to_string(),
        );
    }
    let calculation = calculate_payment(
        &ctx.graph,
        &lead.lead_id,
        &fs.school_code,
        payment_type,
        fs.amount,
        &fs.currency,
        student_count,
    )
    .await?;

    let due_at = Utc::now() + Duration::hours(ctx.default_due_hours);
    let due_iso = due_at.to_rfc3339();
    let obligation_id = format!("FEEOBL-{}", Uuid::new_v4());
    let obligation = fee_obligation_repository::upsert_for_lead(
        &ctx.graph,
        fee_obligation_repository::FeeObligationUpsert {
            tenant_id: ctx.tenant_id,
            lead_id: &lead.lead_id,
            obligation_type: payment_type,
            amount_due: calculation.net_amount,
            currency: &fs.currency,
            due_iso: &due_iso,
            new_id: &obligation_id,
            gross_amount: calculation.gross_amount,
            discount_amount: calculation.discount_amount,
            promotion_code: calculation.promotion_code.as_deref(),
            promotion_rule_id: calculation.promotion_rule_id.as_deref(),
            promotion_snapshot_json: calculation.promotion_snapshot_json.as_deref(),
            line_items_json: &calculation.line_items_json,
        },
    )
    .await
    .map_err(|e| format!("fee obligation upsert failed: {e}"))?;

    let payment_id = format!("PAY-{}", Uuid::new_v4());
    let manual_reference = format!("TWSI-{}", &payment_id.trim_start_matches("PAY-")[..8]);
    let bank = resolve_manual_bank_details(&settings, manual_bank_account_id)?;

    payment_repository::create_manual_pending(
        &ctx.graph,
        &payment_id,
        ctx.tenant_id,
        payment_type,
        calculation.net_amount,
        &fs.currency,
        &due_at.to_rfc3339(),
        &obligation.fee_obligation_id,
        &lead.lead_id,
        &manual_reference,
        &bank,
        calculation.gross_amount,
        calculation.discount_amount,
        calculation.promotion_code.as_deref(),
        calculation.promotion_rule_id.as_deref(),
        calculation.promotion_snapshot_json.as_deref(),
        &calculation.line_items_json,
    )
    .await
    .map_err(|e| format!("manual payment persist failed: {e}"))?;

    set_application_status_for_lead(&ctx.graph, &lead.lead_id, "submitted", "payment_pending")
        .await;

    let payment = payment_repository::find_by_id(&ctx.graph, &payment_id)
        .await
        .map_err(|e| format!("manual payment fetch failed: {e}"))?
        .ok_or_else(|| "manual payment created but not found".to_string())?;

    Ok(ManualPaymentOutcome { payment, settings })
}

/// Preview what this Lead would be charged without actually creating an
/// invoice. Powers the frontend's payment page breakdown:
/// "Rp 1.000.000 × 2 students = Rp 2.000.000".
pub async fn preview_invoice(
    graph: &Graph,
    tenant_id: &str,
    admission_id: &str,
    payment_type: &str,
) -> Result<InvoicePreview, String> {
    let lead = fetch_lead(graph, admission_id)
        .await?
        .ok_or_else(|| "Lead not found".to_string())?;

    let school_id = crate::repositories::school_repository::find_school_id_by_code(
        graph,
        tenant_id,
        &lead.target_school_preference,
    )
    .await
    .map_err(|e| format!("school lookup failed: {e}"))?
    .ok_or_else(|| {
        format!(
            "school not found for code {}",
            lead.target_school_preference
        )
    })?;

    let fs = fee_structure_repository::find_active(graph, tenant_id, &school_id, payment_type)
        .await
        .map_err(|e| format!("fee structure lookup failed: {e}"))?
        .ok_or_else(|| {
            format!(
                "no active FeeStructure for {} / {}",
                school_id, payment_type
            )
        })?;

    // Same per-student-scope rule as create_invoice: Student id or
    // enrolment_fee → always 1; Lead id + application_fee → count kids.
    let is_student_scoped = admission_id.starts_with("STU-") || payment_type == "enrolment_fee";
    let student_count = if is_student_scoped {
        1_i64
    } else {
        count_students_for_lead(graph, admission_id).await?
    };
    let calculation = calculate_payment(
        graph,
        &lead.lead_id,
        &fs.school_code,
        payment_type,
        fs.amount,
        &fs.currency,
        student_count,
    )
    .await?;

    Ok(InvoicePreview {
        school_code: fs.school_code,
        payment_type: payment_type.to_string(),
        unit_amount: fs.amount,
        currency: fs.currency,
        student_count,
        total: calculation.net_amount,
        gross_amount: calculation.gross_amount,
        discount_amount: calculation.discount_amount,
        net_amount: calculation.net_amount,
        promotion_code: calculation.promotion_code,
        promotion_rule_id: calculation.promotion_rule_id,
        line_items: calculation.line_items,
    })
}

pub async fn get_payment_settings(
    graph: &Graph,
    seed: &PaymentSettingsSeed,
) -> Result<PaymentSettings, String> {
    payment_settings_repository::get_or_seed(graph, seed)
        .await
        .map_err(|e| format!("payment settings fetch failed: {e}"))
}

pub async fn update_payment_settings(
    graph: &Graph,
    seed: &PaymentSettingsSeed,
    payload: UpdatePaymentSettings,
    actor: &str,
) -> Result<PaymentSettings, String> {
    let current = get_payment_settings(graph, seed).await?;
    let merged = UpdatePaymentSettings {
        xendit_enabled: payload.xendit_enabled,
        manual_transfer_enabled: payload.manual_transfer_enabled,
        qris_enabled: Some(payload.qris_enabled.unwrap_or(current.qris_enabled)),
        qris_image_url: Some(payload.qris_image_url.unwrap_or(current.qris_image_url)),
        qris_label: Some(payload.qris_label.unwrap_or(current.qris_label)),
        qris_instructions: Some(
            payload
                .qris_instructions
                .unwrap_or(current.qris_instructions),
        ),
        bank_name: Some(payload.bank_name.unwrap_or(current.bank_name)),
        bank_account_name: Some(
            payload
                .bank_account_name
                .unwrap_or(current.bank_account_name),
        ),
        bank_account_number: Some(
            payload
                .bank_account_number
                .unwrap_or(current.bank_account_number),
        ),
        instructions: Some(payload.instructions.unwrap_or(current.instructions)),
        manual_bank_accounts: Some(
            payload
                .manual_bank_accounts
                .unwrap_or(current.manual_bank_accounts),
        ),
    };

    payment_settings_repository::update(graph, &seed.tenant_id, merged, actor)
        .await
        .map_err(|e| format!("payment settings update failed: {e}"))
}

fn resolve_manual_bank_details(
    settings: &PaymentSettings,
    selected_id: Option<&str>,
) -> Result<ManualBankDetails, String> {
    let selected_id = selected_id.map(str::trim).filter(|id| !id.is_empty());
    if selected_id == Some(STATIC_QRIS_ACCOUNT_ID) {
        return resolve_qris_details(settings);
    }

    let enabled_accounts: Vec<&ManualBankAccount> = if settings.manual_transfer_enabled {
        settings
            .manual_bank_accounts
            .iter()
            .filter(|account| account.enabled)
            .collect()
    } else {
        Vec::new()
    };

    let account = if let Some(selected_id) = selected_id {
        enabled_accounts
            .iter()
            .copied()
            .find(|account| account.id == selected_id)
            .ok_or_else(|| "selected manual bank account is not available".to_string())?
    } else if let Some(account) = enabled_accounts.first().copied() {
        account
    } else if settings.manual_transfer_enabled {
        if settings.bank_name.trim().is_empty()
            && settings.bank_account_name.trim().is_empty()
            && settings.bank_account_number.trim().is_empty()
        {
            return Err("manual transfer destination bank account is not configured".to_string());
        }
        return Ok(ManualBankDetails {
            bank_account_id: String::new(),
            bank_name: settings.bank_name.clone(),
            account_name: settings.bank_account_name.clone(),
            account_number: settings.bank_account_number.clone(),
            instructions: settings.instructions.clone(),
        });
    } else if settings.qris_enabled {
        return resolve_qris_details(settings);
    } else {
        return Err("proof-based payment destination is not configured".to_string());
    };

    Ok(ManualBankDetails {
        bank_account_id: account.id.clone(),
        bank_name: account.bank_name.clone(),
        account_name: account.account_name.clone(),
        account_number: account.account_number.clone(),
        instructions: if account.instructions.is_empty() {
            settings.instructions.clone()
        } else {
            account.instructions.clone()
        },
    })
}

fn resolve_qris_details(settings: &PaymentSettings) -> Result<ManualBankDetails, String> {
    if !settings.qris_enabled || settings.qris_image_url.trim().is_empty() {
        return Err("QRIS payment method is not configured".to_string());
    }

    Ok(ManualBankDetails {
        bank_account_id: STATIC_QRIS_ACCOUNT_ID.to_string(),
        bank_name: if settings.qris_label.trim().is_empty() {
            "QRIS".to_string()
        } else {
            settings.qris_label.clone()
        },
        account_name: "Static QRIS".to_string(),
        account_number: String::new(),
        instructions: settings.qris_instructions.clone(),
    })
}

fn can_update_manual_destination(payment: &Payment) -> bool {
    matches!(
        payment.status.as_str(),
        "awaiting_proof" | "underpaid" | "proof_rejected"
    )
}

fn should_update_manual_destination(payment: &Payment, bank: &ManualBankDetails) -> bool {
    payment
        .manual_bank_account_id
        .as_deref()
        .unwrap_or_default()
        != bank.bank_account_id
        || payment
            .bank_name
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        || payment
            .bank_account_name
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        || payment
            .bank_account_number
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
}

pub async fn record_manual_proof(
    graph: &Graph,
    input: CreateProofInput<'_>,
) -> Result<PaymentProof, String> {
    payment_repository::create_payment_proof(graph, input)
        .await
        .map_err(|e| format!("payment proof persist failed: {e}"))
}

pub async fn list_manual_review_rows(
    graph: &Graph,
    filters: PaymentReviewFilters<'_>,
    limit: i64,
    offset: i64,
) -> Result<PaymentReviewList, String> {
    let rows = payment_repository::list_review_rows(graph, filters, limit, offset)
        .await
        .map_err(|e| format!("payment review queue failed: {e}"))?;
    let total = payment_repository::count_review_rows(graph, filters)
        .await
        .map_err(|e| format!("payment review count failed: {e}"))?;
    Ok(PaymentReviewList {
        rows,
        total,
        limit,
        offset,
    })
}

pub async fn get_manual_review_detail(
    graph: &Graph,
    payment_id: &str,
) -> Result<Option<PaymentReviewDetail>, String> {
    payment_repository::find_review_detail(graph, payment_id)
        .await
        .map_err(|e| format!("payment review detail failed: {e}"))
}

pub async fn review_manual_payment(
    graph: &Graph,
    payment_id: &str,
    payload: ReviewManualPaymentRequest,
    actor: &str,
) -> Result<Payment, String> {
    let payment = payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("payment fetch failed: {e}"))?
        .ok_or_else(|| "Payment not found".to_string())?;

    if payment.payment_method.as_deref() != Some("manual_transfer") {
        return Err("payment is not a manual transfer".to_string());
    }
    if payment.status == "paid" {
        return Err("payment is already paid".to_string());
    }

    let due = payment.amount.max(0);
    let current_verified = payment.amount_verified.unwrap_or(0).max(0);
    let submitted = payment.amount_submitted.unwrap_or(0).max(current_verified);
    let note = payload.note.clone().unwrap_or_default();
    let decision = payload.decision.to_lowercase();

    let (payment_status, proof_status, verified, short, overpaid, rejection_reason, receipt_ref) =
        match decision.as_str() {
            "approve" => {
                let verified = payload
                    .verified_amount
                    .unwrap_or(submitted)
                    .max(current_verified);
                if verified < due {
                    return Err(
                        "verified amount is lower than amount due; use underpaid".to_string()
                    );
                }
                (
                    "paid",
                    "approved",
                    verified,
                    0,
                    (verified - due).max(0),
                    "",
                    Some(payment_id),
                )
            }
            "underpaid" => {
                let verified = payload
                    .verified_amount
                    .ok_or_else(|| "verifiedAmount is required for underpaid review".to_string())?;
                if verified <= 0 {
                    return Err("verifiedAmount must be greater than zero".to_string());
                }
                if verified >= due {
                    return Err("verifiedAmount covers the full amount; use approve".to_string());
                }
                (
                    "underpaid",
                    "approved",
                    verified,
                    due - verified,
                    0,
                    "",
                    None,
                )
            }
            "reject" => {
                if note.trim().is_empty() {
                    return Err("review note is required when rejecting proof".to_string());
                }
                let short = (due - current_verified).max(0);
                let status = if current_verified > 0 {
                    "underpaid"
                } else {
                    "proof_rejected"
                };
                (
                    status,
                    "rejected",
                    current_verified,
                    short,
                    0,
                    note.as_str(),
                    None,
                )
            }
            _ => return Err("decision must be approve, underpaid, or reject".to_string()),
        };

    payment_repository::review_manual_payment(
        graph,
        payment_repository::ManualPaymentReviewUpdate {
            payment_id,
            payment_status,
            proof_status,
            amount_verified: verified,
            short_amount: short,
            overpaid_amount: overpaid,
            note: if note.trim().is_empty() {
                None
            } else {
                Some(note.as_str())
            },
            rejection_reason: if rejection_reason.is_empty() {
                None
            } else {
                Some(rejection_reason)
            },
            reviewed_by: actor,
            receipt_ref,
        },
    )
    .await
    .map_err(|e| format!("payment review persist failed: {e}"))?;

    let reviewed = payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("payment fetch after review failed: {e}"))?
        .ok_or_else(|| "Payment not found after review".to_string())?;

    if reviewed.status == "paid" {
        apply_paid_side_effects(graph, &reviewed).await?;
    }

    Ok(reviewed)
}

#[derive(Debug, serde::Serialize)]
pub struct InvoicePreview {
    #[serde(rename = "schoolCode")]
    pub school_code: String,
    #[serde(rename = "paymentType")]
    pub payment_type: String,
    /// Fee per student, as published by the school's FeeStructure.
    #[serde(rename = "unitAmount")]
    pub unit_amount: i64,
    pub currency: String,
    #[serde(rename = "studentCount")]
    pub student_count: i64,
    /// Net due amount. Kept for backwards-compatible frontend readers.
    pub total: i64,
    #[serde(rename = "grossAmount")]
    pub gross_amount: i64,
    #[serde(rename = "discountAmount")]
    pub discount_amount: i64,
    #[serde(rename = "netAmount")]
    pub net_amount: i64,
    #[serde(rename = "promotionCode", skip_serializing_if = "Option::is_none")]
    pub promotion_code: Option<String>,
    #[serde(rename = "promotionRuleId", skip_serializing_if = "Option::is_none")]
    pub promotion_rule_id: Option<String>,
    #[serde(rename = "lineItems")]
    pub line_items: Vec<PaymentLineItem>,
}

/// Conditionally advance `Application.status` for the Application bound
/// to this Lead. Matches only if the current status is `from_status`
/// (idempotent — a double-fire from Xendit webhook + poll doesn't
/// re-transition). Best-effort: if there's no Application yet (first
/// invoice created before students were submitted, or non-standard
/// flow) the query just matches zero rows and we move on.
///
/// Owned by admission-service, but we do this via direct cypher since
/// the graph is shared and it avoids a synchronous HTTP dependency.
async fn set_application_status_for_lead(
    graph: &Graph,
    lead_id: &str,
    from_status: &str,
    to_status: &str,
) {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$lead_id})-[:CONVERTED_TO]->(a:Application) \
         WHERE a.status = $from \
         SET a.status = $to, a.updated_at = datetime() \
         RETURN a.application_id AS id"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string())
    .param("from", from_status.to_string())
    .param("to", to_status.to_string());

    match graph.execute(q).await {
        Ok(mut res) => {
            // Drain so the transaction commits. Don't care about the row.
            let _ = res.next().await;
            info!(lead_id=%lead_id, from=%from_status, to=%to_status, "application status advanced (best-effort)");
        }
        Err(e) => {
            warn!(lead_id=%lead_id, error=%e, "failed to advance application status; payment flow continues");
        }
    }
}

/// Cascade every ApplicantStudent under a Lead from `submitted` to
/// `test_pending` once the application fee is paid. Per spec §4.4,
/// passing the fee is what gates access to the entrance test.
///
/// The cypher is idempotent and no-op for students that are already
/// past `submitted` (e.g. an admin-advanced student, or a re-payment).
/// When an enrolment_fee Payment is confirmed paid, advance every
/// ApplicantStudent under the Lead that's in `offer_accepted` all the
/// way to `handed_to_sis`, creating the EnrolledStudent node and the
/// (ApplicantStudent)-[:ENROLLED_AS]->(EnrolledStudent) edge per
/// spec §2.2 (admissions + SIS linked, not overwritten).
///
/// student_id is `STU-<uuid>` and student_number is `{SCHOOL}{YEAR}-<4hex>`.
/// For MVP we compute both inline in cypher using randomUUID() +
/// current-year toString — good enough until SIS gets its own service
/// with a real sequencing store.
///
/// Idempotent via MERGE; if the kid is already enrolled the query
/// no-ops.
async fn cascade_students_on_enrolment_paid(graph: &Graph, lead_id: &str) {
    let q = Query::new(
        "MATCH (:Lead {lead_id: $lead_id})-[:HAS_STUDENT]->(s:Student) \
         WHERE coalesce(s.applicantStatus, '') = 'offer_accepted' \
         OPTIONAL MATCH (s)-[:HAS_OFFER]->(o:Offer) \
         WITH s, o, randomUUID() AS uid, toString(date().year) AS yyyy \
         MERGE (e:EnrolledStudent {applicant_student_id: s.studentId}) \
         ON CREATE SET e.student_id = 'STU-' + uid, \
                       e.student_number = coalesce(replace(o.target_school_id, 'SCH-', ''), 'DS') + yyyy + '-' + toUpper(substring(uid, 0, 4)), \
                       e.tenant_id = 'TENANT-001', \
                       e.school_id = coalesce(o.target_school_id, ''), \
                       e.year_group = coalesce(o.target_year_group, ''), \
                       e.status = 'active', \
                       e.enrolment_date = toString(date()), \
                       e.created_at = datetime(), e.updated_at = datetime() \
         ON MATCH SET e.updated_at = datetime() \
         MERGE (s)-[:ENROLLED_AS]->(e) \
         SET s.applicantStatus = 'handed_to_sis', s.updatedAt = datetime() \
         RETURN count(s) AS enrolled_count".to_string(),
    )
    .param("lead_id", lead_id.to_string());
    match graph.execute(q).await {
        Ok(mut res) => {
            let _ = res.next().await;
            info!(lead_id=%lead_id, "students cascaded → handed_to_sis (EnrolledStudent created)");
        }
        Err(e) => {
            warn!(lead_id=%lead_id, error=%e, "failed to enrol students on enrolment_paid");
        }
    }
}

async fn cascade_students_to_test_pending(graph: &Graph, lead_id: &str) {
    let q = Query::new(
        "MATCH (:Lead {lead_id:$lead_id})-[:HAS_STUDENT]->(s:Student) \
         WHERE coalesce(s.applicantStatus, 'submitted') = 'submitted' \
         SET s.applicantStatus = 'test_pending', s.updatedAt = datetime()"
            .to_string(),
    )
    .param("lead_id", lead_id.to_string());
    if let Err(e) = graph.run(q).await {
        warn!(lead_id=%lead_id, error=%e, "failed to cascade students to test_pending");
    } else {
        info!(lead_id=%lead_id, "students cascaded → test_pending");
    }
}

async fn count_students_for_lead(graph: &Graph, admission_id: &str) -> Result<i64, String> {
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$id})-[:HAS_STUDENT]->(s:Student) RETURN count(s) AS n".to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("student count: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("student count row: {e}"))?
    {
        Ok(row.get::<i64>("n").unwrap_or(0))
    } else {
        Ok(0)
    }
}

pub async fn fetch_payment(graph: &Graph, payment_id: &str) -> Result<Option<Payment>, String> {
    payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("payment fetch failed: {e}"))
}

pub async fn payment_notification_context(
    graph: &Graph,
    payment: &Payment,
) -> Result<Option<PaymentNotificationContext>, String> {
    let Some(lead_id) = payment.lead_id.as_deref() else {
        return Ok(None);
    };
    let Some(lead) = fetch_lead(graph, lead_id).await? else {
        return Ok(None);
    };
    Ok(Some(PaymentNotificationContext {
        parent_name: lead.parent_name,
        email: lead.email,
        whatsapp: lead.whatsapp,
        school: lead.target_school_preference,
    }))
}

pub async fn resolve_notification_channels(
    graph: &Graph,
    school_code: &str,
    event: &str,
) -> Result<Vec<String>, String> {
    let key = school_code
        .trim()
        .trim_start_matches("SCH-")
        .trim_start_matches("sch-")
        .to_uppercase();
    if key.is_empty() {
        return Ok(default_notification_channels_for_event(event));
    }
    let q = Query::new(
        "MATCH (s:AdmissionsSettings) \
         WHERE toUpper(s.school_id) = $key \
            OR toUpper(s.school_id) = 'SCH-' + $key \
         RETURN s.notification_channels_json AS channelsJson \
         ORDER BY s.academic_year DESC, s.updated_at DESC \
         LIMIT 1"
            .to_string(),
    )
    .param("key", key);
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("notification channels fetch: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("notification channels row: {e}"))?
    {
        let channels_json = row.get::<String>("channelsJson").unwrap_or_default();
        let preferences = parse_notification_channels(&channels_json);
        return Ok(channels_for_event(&preferences, event));
    }
    Ok(default_notification_channels_for_event(event))
}

pub async fn mark_payment_notification_queued(
    graph: &Graph,
    payment_id: &str,
    event: &str,
    channel: &str,
) -> Result<bool, String> {
    let property = match (event, channel) {
        ("payment_approved", "email") => "payment_approved_email_queued_at",
        ("payment_approved", "whatsapp") => "payment_approved_whatsapp_queued_at",
        ("payment_underpaid", "email") => "payment_underpaid_email_queued_at",
        ("payment_underpaid", "whatsapp") => "payment_underpaid_whatsapp_queued_at",
        ("payment_rejected", "email") => "payment_rejected_email_queued_at",
        ("payment_rejected", "whatsapp") => "payment_rejected_whatsapp_queued_at",
        _ => {
            return Err(format!(
                "unsupported payment notification event/channel: {event}/{channel}"
            ))
        }
    };
    let now = Utc::now().to_rfc3339();
    let cypher = format!(
        "MATCH (p:Payment {{payment_id: $payment_id}}) \
         WHERE p.{property} IS NULL \
         SET p.{property} = datetime($now) \
         RETURN true AS marked"
    );
    let q = Query::new(cypher)
        .param("payment_id", payment_id.to_string())
        .param("now", now);
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("mark payment notification: {e}"))?;
    Ok(result
        .next()
        .await
        .map_err(|e| format!("mark payment notification row: {e}"))?
        .is_some())
}

fn default_notification_channels_for_event(_event: &str) -> Vec<String> {
    vec![NOTIFICATION_CHANNEL_EMAIL.to_string()]
}

fn parse_notification_channels(value: &str) -> Vec<NotificationChannelPreference> {
    serde_json::from_str::<Vec<NotificationChannelPreference>>(value).unwrap_or_default()
}

fn channels_for_event(preferences: &[NotificationChannelPreference], event: &str) -> Vec<String> {
    let mut channels = preferences
        .iter()
        .find(|preference| preference.event == event)
        .map(|preference| normalize_notification_channels(&preference.channels))
        .unwrap_or_default();
    if channels.is_empty() {
        channels.push(NOTIFICATION_CHANNEL_EMAIL.to_string());
    }
    channels
}

fn normalize_notification_channels(channels: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for channel in channels {
        let canonical = match channel.trim().to_ascii_lowercase().as_str() {
            "email" | "mail" => NOTIFICATION_CHANNEL_EMAIL,
            "whatsapp" | "wa" | "phone" => NOTIFICATION_CHANNEL_WHATSAPP,
            _ => continue,
        };
        if !out.iter().any(|value| value == canonical) {
            out.push(canonical.to_string());
        }
    }
    out
}

/// Refresh a pending payment by asking Xendit for the authoritative status.
///
/// Why: In dev/test/staging our webhook URL isn't publicly reachable, so
/// Xendit can't call us when the invoice is paid. The frontend polls this
/// endpoint instead; on each poll we ask Xendit for the truth and persist.
///
/// No-op when the local status is already terminal (paid/expired/failed) or
/// when we have no Xendit invoice id to query.
pub async fn fetch_payment_refreshed(
    graph: &Graph,
    xendit: &XenditClient,
    payment_id: &str,
) -> Result<Option<Payment>, String> {
    let Some(current) = fetch_payment(graph, payment_id).await? else {
        return Ok(None);
    };

    if current.status != "pending" {
        return Ok(Some(current));
    }
    let Some(invoice_id) = current.invoice_ref.clone() else {
        return Ok(Some(current));
    };
    if invoice_id.is_empty() {
        return Ok(Some(current));
    }

    match xendit.get_invoice(&invoice_id).await {
        Ok(fresh) => {
            let upstream = fresh.status.to_uppercase();
            match upstream.as_str() {
                "PAID" | "SETTLED" => {
                    if let Err(e) = payment_repository::mark_paid(
                        graph,
                        payment_id,
                        fresh
                            .payment_method
                            .as_deref()
                            .or(fresh.payment_channel.as_deref()),
                        fresh.payment_id.as_deref().or(Some(&fresh.id)),
                    )
                    .await
                    {
                        warn!(error=%e, "xendit refresh: mark_paid failed");
                    } else {
                        // Best-effort settle the linked FeeObligation.
                        let _ = settle_obligation_for_payment(graph, payment_id).await;

                        // Payment-type-specific cascades. application_fee
                        // moves us into the test phase; enrolment_fee
                        // closes the admissions funnel and hands the
                        // applicant to SIS.
                        if let Some(lead_id) = current.lead_id.as_deref() {
                            match current.payment_type.as_str() {
                                "application_fee" => {
                                    set_application_status_for_lead(
                                        graph,
                                        lead_id,
                                        "submitted",
                                        "application_fee_paid",
                                    )
                                    .await;
                                    set_application_status_for_lead(
                                        graph,
                                        lead_id,
                                        "payment_pending",
                                        "application_fee_paid",
                                    )
                                    .await;
                                    cascade_students_to_test_pending(graph, lead_id).await;
                                }
                                "enrolment_fee" => {
                                    set_application_status_for_lead(
                                        graph,
                                        lead_id,
                                        "offer_stage",
                                        "completed",
                                    )
                                    .await;
                                    cascade_students_on_enrolment_paid(graph, lead_id).await;
                                }
                                _ => {
                                    // term_fee, capital_levy etc. — no
                                    // admissions-funnel side-effect.
                                }
                            }
                        }
                        info!(payment_id=%payment_id, "xendit refresh: marked paid");
                    }
                }
                "EXPIRED" => {
                    if let Err(e) =
                        payment_repository::mark_status(graph, payment_id, "expired").await
                    {
                        warn!(error=%e, "xendit refresh: mark_status expired failed");
                    } else {
                        info!(payment_id=%payment_id, "xendit refresh: marked expired");
                    }
                }
                "PENDING" => { /* no change */ }
                other => {
                    warn!(status=%other, payment_id=%payment_id, "xendit refresh: unknown status");
                }
            }
        }
        Err(e) => {
            // Don't fail the whole request; return the stale local view.
            warn!(error=%e, payment_id=%payment_id, "xendit refresh failed, returning local state");
        }
    }

    fetch_payment(graph, payment_id).await
}

pub async fn handle_webhook(
    graph: &Graph,
    payment_id: &str,
    gateway_status: &str,
    payment_method: Option<&str>,
    receipt_ref: Option<&str>,
) -> Result<(), String> {
    // Xendit statuses: PAID / EXPIRED / PENDING / SETTLED
    let normalized = gateway_status.to_uppercase();
    let payment = payment_repository::find_by_id(graph, payment_id)
        .await
        .map_err(|e| format!("find failed: {e}"))?
        .ok_or_else(|| format!("payment {payment_id} not found"))?;

    match normalized.as_str() {
        "PAID" | "SETTLED" => {
            payment_repository::mark_paid(graph, payment_id, payment_method, receipt_ref)
                .await
                .map_err(|e| format!("mark_paid failed: {e}"))?;
            // Also settle any FeeObligation linked via SETTLED_BY relationship
            settle_obligation_for_payment(graph, payment_id).await?;
            // And advance the Application lifecycle + per-child
            // ApplicantStudent status based on which fee was paid.
            if let Some(lead_id) = payment.lead_id.as_deref() {
                match payment.payment_type.as_str() {
                    "application_fee" => {
                        set_application_status_for_lead(
                            graph,
                            lead_id,
                            "submitted",
                            "application_fee_paid",
                        )
                        .await;
                        set_application_status_for_lead(
                            graph,
                            lead_id,
                            "payment_pending",
                            "application_fee_paid",
                        )
                        .await;
                        cascade_students_to_test_pending(graph, lead_id).await;
                    }
                    "enrolment_fee" => {
                        set_application_status_for_lead(graph, lead_id, "offer_stage", "completed")
                            .await;
                        cascade_students_on_enrolment_paid(graph, lead_id).await;
                    }
                    _ => {}
                }
            }
        }
        "EXPIRED" => {
            payment_repository::mark_status(graph, payment_id, "expired")
                .await
                .map_err(|e| format!("mark_status failed: {e}"))?;
        }
        "PENDING" => {
            // no-op; already pending
        }
        other => {
            warn!(status=%other, payment_id=%payment_id, "unknown webhook status");
        }
    }
    let _ = payment;
    Ok(())
}

async fn apply_paid_side_effects(graph: &Graph, payment: &Payment) -> Result<(), String> {
    settle_obligation_for_payment(graph, &payment.payment_id).await?;
    if let Some(lead_id) = payment.lead_id.as_deref() {
        match payment.payment_type.as_str() {
            "application_fee" => {
                set_application_status_for_lead(
                    graph,
                    lead_id,
                    "submitted",
                    "application_fee_paid",
                )
                .await;
                set_application_status_for_lead(
                    graph,
                    lead_id,
                    "payment_pending",
                    "application_fee_paid",
                )
                .await;
                cascade_students_to_test_pending(graph, lead_id).await;
            }
            "enrolment_fee" => {
                set_application_status_for_lead(graph, lead_id, "offer_stage", "completed").await;
                cascade_students_on_enrolment_paid(graph, lead_id).await;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn settle_obligation_for_payment(graph: &Graph, payment_id: &str) -> Result<(), String> {
    let q = Query::new(
        "MATCH (f:FeeObligation)-[:SETTLED_BY]->(p:Payment {payment_id:$pid}) \
         SET f.status = 'paid', f.paid_at = datetime() \
         RETURN f.fee_obligation_id AS id"
            .to_string(),
    )
    .param("pid", payment_id.to_string());
    let mut res = graph
        .execute(q)
        .await
        .map_err(|e| format!("settle failed: {e}"))?;
    let _ = res.next().await;
    Ok(())
}

fn pretty_payment_type(ty: &str) -> &str {
    match ty {
        "application_fee" => "Registration fee",
        "enrolment_fee" => "Enrolment fee",
        "capital_levy" => "Capital levy",
        "term_fee" => "Term fee",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{calculate_discount_amount, PromotionRuleSnapshot};

    fn rule(discount_type: &str, discount_value: i64) -> PromotionRuleSnapshot {
        PromotionRuleSnapshot {
            promotion_code: "MKT-NADIA".to_string(),
            promotion_rule_id: "PROMO-1".to_string(),
            source: "reference_code".to_string(),
            discount_type: discount_type.to_string(),
            discount_value,
            max_discount_amount: None,
            min_net_amount: None,
        }
    }

    #[test]
    fn fixed_amount_discount_cannot_exceed_gross() {
        let mut rule = rule("fixed_amount", 500_000);
        assert_eq!(calculate_discount_amount(2_000_000, &rule), 500_000);
        rule.discount_value = 3_000_000;
        assert_eq!(calculate_discount_amount(2_000_000, &rule), 2_000_000);
    }

    #[test]
    fn percent_discount_honors_max_cap() {
        let mut rule = rule("percent", 25);
        rule.max_discount_amount = Some(300_000);
        assert_eq!(calculate_discount_amount(2_000_000, &rule), 300_000);
    }

    #[test]
    fn min_net_amount_limits_discount() {
        let mut rule = rule("fixed_amount", 900_000);
        rule.min_net_amount = Some(1_500_000);
        assert_eq!(calculate_discount_amount(2_000_000, &rule), 500_000);
    }

    #[test]
    fn unsupported_discount_type_is_zero() {
        let rule = rule("free_text", 100);
        assert_eq!(calculate_discount_amount(2_000_000, &rule), 0);
    }
}

// ---- internal helpers ----

struct LeadSnapshot {
    lead_id: String,
    parent_name: String,
    email: String,
    whatsapp: String,
    target_school_preference: String,
}

/// Resolve an `admissionId` to the owning Lead. Accepts either a
/// `LEAD-xxx` id directly, or a `Student.studentId` — in which case
/// we walk back through the `HAS_STUDENT` edge to the parent Lead.
/// This lets the enrolment_fee flow (where the offer is per-student)
/// reuse the same `/invoice` endpoint as the application_fee flow.
async fn fetch_lead(graph: &Graph, admission_id: &str) -> Result<Option<LeadSnapshot>, String> {
    // Try it as a Lead id first — the common case for application_fee.
    let q = Query::new(
        "MATCH (l:Lead {lead_id:$id}) \
         RETURN l.lead_id AS lead_id, l.parent_name AS parent_name, l.email AS email, \
                coalesce(l.whatsapp, l.mobile, '') AS whatsapp, \
                l.target_school_preference AS school \
         LIMIT 1"
            .to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("lead fetch: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("lead fetch row: {e}"))?
    {
        return Ok(Some(LeadSnapshot {
            lead_id: row.get("lead_id").unwrap_or_default(),
            parent_name: row.get("parent_name").unwrap_or_default(),
            email: row.get("email").unwrap_or_default(),
            whatsapp: row.get("whatsapp").unwrap_or_default(),
            target_school_preference: row.get("school").unwrap_or_default(),
        }));
    }

    // Fall back: treat it as a Student id and walk back to the Lead.
    // Enrolment-fee flow passes the student id because the Offer is
    // per-kid, not per-application.
    let q = Query::new(
        "MATCH (l:Lead)-[:HAS_STUDENT]->(Student {studentId:$id}) \
         RETURN l.lead_id AS lead_id, l.parent_name AS parent_name, l.email AS email, \
                coalesce(l.whatsapp, l.mobile, '') AS whatsapp, \
                l.target_school_preference AS school \
         LIMIT 1"
            .to_string(),
    )
    .param("id", admission_id.to_string());
    let mut result = graph
        .execute(q)
        .await
        .map_err(|e| format!("lead fetch by student: {e}"))?;
    if let Some(row) = result
        .next()
        .await
        .map_err(|e| format!("lead fetch by student row: {e}"))?
    {
        Ok(Some(LeadSnapshot {
            lead_id: row.get("lead_id").unwrap_or_default(),
            parent_name: row.get("parent_name").unwrap_or_default(),
            email: row.get("email").unwrap_or_default(),
            whatsapp: row.get("whatsapp").unwrap_or_default(),
            target_school_preference: row.get("school").unwrap_or_default(),
        }))
    } else {
        Ok(None)
    }
}
