# DOKU offer-payment operations

This integration is intentionally fail-closed. DOKU never falls back to
another provider when it is unavailable. A school may separately enable the
proof-based manual bank-transfer/static-QRIS flow as an explicit parent choice.
Both choices use only the accepted immutable
`OfferPricingSnapshot.amountDueNow`; Xendit is not exposed for offer payments.
The DOKU admin toggle defaults off and controls both parent visibility and
checkout authorization. Provider credentials remain server-managed and are
never stored or displayed in the admin UI. Xendit is forced off in the
settings contract.
The service reserves one tenant/offer/revision/snapshot payment slot before
calling DOKU or creating a manual payment, so concurrent choices cannot create
two active obligations.

Manual proof submission is not payment confirmation. It moves the payment to
`pending_verification`; only an authorized Finance approval may mark it paid
and run the offer-payment side effects. Admissions roles may view the review
queue but cannot confirm money.

## Non-production prerequisites

- DOKU sandbox merchant Client ID and Secret Key.
- Activated DOKU Checkout product and an agreed channel allowlist.
- Public HTTPS notification URL ending in
  `/api/v1/payments/webhook/doku`.
- HTTPS browser return URL. The return page is informational only.
- DOKU dashboard notification/signature configuration matching the URL.
- DOKU non-SNAP Check Status API entitlement (existing merchants may need
  DOKU support to activate it).
- A synthetic accepted offer revision with an `offer-pricing-v1` snapshot.

Configure without committing values:

```text
DOKU_API_URL=https://api-sandbox.doku.com
DOKU_CLIENT_ID=<sandbox client id>
DOKU_SECRET_KEY=<sandbox secret key>
DOKU_RETURN_URL=https://<non-production-host>/parent/payments
DOKU_NOTIFICATION_URL=https://<public-non-production-api>/api/v1/payments/webhook/doku
DOKU_PAYMENT_METHOD_TYPES=VIRTUAL_ACCOUNT_BCA,VIRTUAL_ACCOUNT_MANDIRI,QRIS
LEGACY_PARENT_PAYMENTS_ENABLED=false
```

## Verification and reconciliation

Checkout requests use the DOKU non-SNAP HMAC-SHA256 component contract. The
webhook verifies Client ID, request ID, UTC timestamp freshness, request
target, body digest, signature, invoice reference, amount, currency, provider,
and replay key before changing payment state. A browser return or ordinary
payment GET never confirms a DOKU payment.

Finance-authorized reconciliation calls DOKU's signed non-SNAP
`GET /orders/v1/status/{invoice}` endpoint, then runs the returned order through
the same reference, amount, currency, provider, idempotency, and paid-state
side effects as a verified webhook. It is exposed only at
`POST /api/v1/payments/admin/payments/{paymentId}/reconcile/doku`; the merchant
must have Check Status activated before this recovery path is available.
Until that entitlement is confirmed, a missing webhook is an operational
exception; do not mark it paid from the browser or database console.

## Rollout and rollback

1. Exercise signature, mismatch, expiry, and replay cases with synthetic data.
2. Run one sandbox checkout for each enabled channel and reconcile it to the
   stored offer snapshot hash.
3. Enable for one non-production school, monitor failed outbox/payment states,
   then widen.
4. Roll back DOKU by disabling its accepted-offer entry point at the
   frontend/API release boundary. Existing DOKU payments remain
   webhook/reconciliation-owned. Disable offer manual transfer through the
   existing school Payment Method Settings; do not automatically select it
   when DOKU fails.
5. `LEGACY_PARENT_PAYMENTS_ENABLED` defaults to `false` and is the explicit,
   time-bounded rollback switch for the old application-fee Xendit/manual
   surface. It does not govern the snapshot-bound offer manual endpoint and
   must never expose Xendit on an offer.
