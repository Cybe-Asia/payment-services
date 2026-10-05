MATCH (u:User {id:$payer})-[:HAS_APPLICATION]->(l:Lead)-[:HAS_STUDENT]->(s:Student {studentId:$student})
WHERE coalesce(u.role,'parent')='parent'
  AND coalesce(u.staffMemberId,'')=''
  AND coalesce(u.staff_member_id,'')=''
  AND all(role IN coalesce(u.roles,[]) WHERE role='parent')
  AND all(role IN coalesce(u.marketingRoles,[]) WHERE role='parent')
  AND NOT (u)-[:STAFF_PROFILE]->() AND NOT (u)-[:STAFF_MEMBER]->()
  AND NOT EXISTS { MATCH (staff:StaffMember) WHERE toLower(staff.email)=toLower(u.email) }
  AND coalesce(u.email,'')<>'' AND toLower(u.email)=toLower(l.email)
  AND (l.email_otp_verified_at IS NOT NULL OR coalesce(l.otp_verified,false)=true
       OR EXISTS { MATCH (proof:ParentLoginChallenge {purpose:'parent_login',userId:u.id,leadId:l.lead_id,email:toLower(u.email),channel:'email',isUsed:true,delivered:true}) })
  AND NOT EXISTS { MATCH (other_user:User {id:$payer}) WHERE other_user<>u }
  AND NOT EXISTS { MATCH (other_student:Student {studentId:$student}) WHERE other_student<>s }
WITH DISTINCT u,s
MATCH (s)-[:ENROLLED_AS]->(enrollment:EnrolledStudent {status:'active'})-[:ENROLLED_IN]->(section:Section {status:'active'})
WHERE enrollment.applicant_student_id=s.studentId
  AND NOT EXISTS { MATCH (s)-[:ENROLLED_AS]->(other:EnrolledStudent {status:'active'}) WHERE other<>enrollment }
  AND coalesce(enrollment.student_id,'')<>''
  AND enrollment.school_id=section.school_id AND enrollment.tenant_id=section.tenant_id
  AND coalesce(section.school_id,'')<>'' AND coalesce(section.tenant_id,'')<>''
WITH u,s,collect(DISTINCT enrollment) AS enrollments,collect(DISTINCT section) AS sections
WHERE size(enrollments)=1 AND size(sections)=1
WITH u,s,head(enrollments) AS e,head(sections) AS sec
MATCH (school:School {school_id:e.school_id,tenant_id:e.tenant_id})
WHERE NOT EXISTS { MATCH (other_school:School {school_id:e.school_id,tenant_id:e.tenant_id}) WHERE other_school<>school }
WITH DISTINCT u,s,e,sec,school
