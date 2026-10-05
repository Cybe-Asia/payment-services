//! Financial access belongs to the original invoice payer, independently of
//! later enrollment/section changes. This does not authorize new issuance or
//! academic access. Every v1 issue audit was created by the eligibility-guarded
//! Payment transaction; an unaudited/imported invoice is never evidence of it.
pub fn guard() -> String {
    "MATCH (u:User {id:$subject}) \
WHERE u.id=i.payer_user_id AND coalesce(u.role,'parent')='parent' \
AND coalesce(u.staffMemberId,'')='' AND coalesce(u.staff_member_id,'')='' \
AND all(role IN coalesce(u.roles,[]) WHERE role='parent') \
AND all(role IN coalesce(u.marketingRoles,[]) WHERE role='parent') \
AND NOT (u)-[:STAFF_PROFILE]->() AND NOT (u)-[:STAFF_MEMBER]->() \
AND NOT EXISTS {MATCH(staff:StaffMember) WHERE toLower(staff.email)=toLower(u.email)} \
AND NOT EXISTS {MATCH(other:User {id:$subject}) WHERE other<>u} \
AND datetime.realtime().epochSeconds < $expires \
AND EXISTS {MATCH(a:SchoolInvoiceAudit {invoice_id:i.id,action:'issued',version:0}) \
WHERE a.actor_id=i.issued_by AND a.created_at=i.issued_at \
AND i.issued_at <= datetime.realtime() AND coalesce(i.issued_by,'')<>'' \
AND NOT EXISTS {MATCH(other:SchoolInvoiceAudit {invoice_id:i.id,action:'issued'}) WHERE other<>a} \
AND (a.eligibility_contract IS NULL OR \
(a.eligibility_contract='sis-current-family-v1' \
AND a.payer_user_id=i.payer_user_id AND a.student_id=i.student_id \
AND a.enrolled_student_id=i.enrolled_student_id \
AND a.school_id=i.school_id AND a.tenant_id=i.tenant_id \
AND a.request_hash=i.request_hash))} "
        .into()
}
