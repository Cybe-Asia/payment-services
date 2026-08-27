# Payment evidence encryption

Manual-transfer evidence is encrypted with AES-256-GCM before MinIO storage.
Each object uses a random nonce and authenticated context bound to its object
key. After finance-role or parent-ownership authorization, payment-service
decrypts and streams the bytes with `private, no-store` and `nosniff` headers;
it no longer returns object-storage URLs.

The service uses the same runtime contract as admission-service:

```text
DOCUMENT_ENCRYPTION_PRIMARY_KEY_ID=key-YYYY-N
DOCUMENT_ENCRYPTION_KEYRING=key-YYYY-N=<64 lowercase hex characters>[,older-key=<64 hex>]
DOCUMENT_LEGACY_PLAINTEXT_READS_ALLOWED=false
DOCUMENT_ENCRYPTION_MIGRATE_ON_STARTUP=false
```

MinIO configuration without a valid keyring fails startup. For an approved
legacy migration, first back up and inventory `school-test/payments/`, then set
both migration flags to `true` for one controlled rollout. Startup idempotently
rewrites plaintext evidence and authenticates existing envelopes. After
authorized download checks pass, restore both flags to `false` and roll out
again. Never log the keyring, evidence bytes, object URLs, document hashes, or
bank-reference values.

Payment evidence is deliberately excluded from automatic offer-decline erasure:
it is financial audit material and follows the school's accounting retention
policy. Any deletion of payment evidence needs its own approved retention/legal
workflow.
