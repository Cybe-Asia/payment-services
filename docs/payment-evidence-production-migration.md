# Payment evidence migration

`--audit-document-encryption` audits only `school-test/payments/` in the configured
bucket and exits before graph initialization, fee seeding or notification workers.
It is strictly read-only even when the startup migration environment flag is true.

Before production migration, grant the scoped service account
`s3:GetBucketVersioning` and complete that policy sync. Preserve a recoverable
backup and the encryption key outside application storage. Keep
`DOCUMENT_LEGACY_PLAINTEXT_READS_ALLOWED=false`.

Stop legacy writers before enabling `DOCUMENT_ENCRYPTION_MIGRATE_ON_STARTUP` for
one controlled rollout. Migration refuses both enabled and suspended bucket
versioning, authenticates existing ciphertext, encrypts plaintext directly without
enabling HTTP plaintext reads, conditionally replaces the observed ETag, and
rereads/decrypts to compare with original bytes. A failure stops startup; rerunning
is resumable. No unrelated document prefix is modified.

After the audit succeeds, turn startup migration off and restore the ordinary
rollout strategy. A plaintext-only backend image is not a valid rollback after
migration; retain an encryption-capable artifact and the same keyring.

Verification: 28 unit tests pass. Disposable MinIO integration covers strict
plaintext rejection, migration/resume, unrelated namespace preservation, stale
ETag denial, tamper detection, and enabled/suspended versioning refusal.
