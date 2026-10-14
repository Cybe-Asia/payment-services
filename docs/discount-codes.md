# Discount codes on invoices

Status: local implementation, pending review/deployment
Last updated: 2026-10-13

Staff create discount codes in admission-service (`/admin/discount-codes`,
see that service's `docs/api/discount-codes.md`). A family types one into the
EOI; admission-service stores it on the Lead as `discount_code`. Its status
is `pending` until the family verifies a contact; then admission-service
takes one of the code's redemptions and sets `discount_code_status =
'verified'` (or `unverified` when the code is no longer in force or is
full). This service applies a verified code when it computes an invoice. No
endpoint or response shape changes.

## Where it applies

`calculate_payment`, the one place promotions are applied: the invoice
preview, the parent/staff manual-transfer invoice and the (disabled) Xendit
invoice, for payment types `application_fee` and `enrolment_fee`.

Offer payments (`offer_due_now`) are not computed here. They bill the
accepted offer's pricing snapshot unchanged; a discount code for the offer
commitment is applied by admission-service when it prices the offer and
arrives inside that snapshot.

## The rule

A Lead's discount code is one more promotion candidate, next to the promotion
staff attached to the Lead, the Lead's reference-code promotion and the
global ladder (`promotion_repository::find_candidates_for_lead`).

It is a candidate only if, at the moment the invoice is computed:

- `Lead.discount_code_status = 'verified'` (not `pending`, not `unverified`);
- the code (`ReferenceCode`) is `active` and is not a Parent's referral code;
- its rule is `active`, has an approver, and its `payment_type_scope` is the
  payment type being invoiced. The rule is the `DiscountRule` of a discount
  code (`HAS_DISCOUNT_RULE`), or the `PromotionRule` of a marketing code that
  the family typed into the discount field;
- a discount rule with a fixed amount is for `application_fee` only (one
  invoice per family). On any other payment type it is not a candidate;
  admission-service refuses to create such a code in the first place;
- now is inside the rule's window `[valid_from, valid_until)` — from the
  start instant, no longer at the end instant; a missing bound is open, an
  unreadable one fails closed;
- the Lead holds a redemption of the code
  (`(:ReferenceCode)-[:HAS_REDEMPTION]->(:DiscountRedemption {lead_id})`).
  admission-service creates that record atomically against the rule's
  `max_redemptions`, when the family verifies a contact, so a limited code
  never discounts more families than it allows. This service never creates
  or counts redemptions.

Promotions never stack. Each candidate is priced on the invoice's gross and
the largest reduction wins: a family with both a reference-code promotion and
a discount code gets the better of the two, never both. On a tie the recorded
one is, in order: the promotion staff attached, the discount code, the global
ladder, the reference code.

A discount code is never applied as a reference-code promotion. The
reference-code lookup reads `(:ReferenceCode)-[:HAS_PROMOTION_RULE]->(:PromotionRule)`
of marketing codes; a discount code's rule is a `DiscountRule` behind
`HAS_DISCOUNT_RULE` and is not reachable from it, so a discount code typed
into the referral field cannot escape its window and limit. The same holds
for the image from before discount codes, which has only that lookup.

## Amounts and rounding

Amounts are whole rupiah (`i64`; IDR is handled without a minor unit).
Integer arithmetic only.

- Gross = unit fee × number of students on the invoice.
- `percent`: the value is capped at 100; discount = `gross × percent ÷ 100`
  with integer division, i.e. **rounded down to the rupiah**. A rounding
  difference of less than one rupiah stays on the invoice.
- `fixed_amount`: the value, once per invoice (not per student). A discount
  code can have a fixed amount for the application fee only, which is one
  invoice per family.
- The discount is capped at the gross: the total is never negative.
- Net = gross − discount.

Worked examples (application fee):

| Gross | Candidates | Applied | Discount | Net |
| --- | --- | --- | --- | --- |
| 1 child × Rp 1.250.000 | code 10 % | code | Rp 125.000 | Rp 1.125.000 |
| 2 children × Rp 1.250.000 = Rp 2.500.000 | code 10 % | code | Rp 250.000 | Rp 2.250.000 |
| 2 children × Rp 1.333.333 = Rp 2.666.666 | code 15 % (= 399.999,9) and reference code Rp 100.000 | code | Rp 399.999 | Rp 2.266.667 |
| Rp 1.250.000 | code 5 % (= Rp 62.500) and reference code Rp 100.000 | reference code | Rp 100.000 | Rp 1.150.000 |
| Rp 999.999 | code 33 % (= 329.999,67) | code | Rp 329.999 | Rp 670.000 |
| Rp 1.250.000 | code fixed Rp 2.000.000 | code | Rp 1.250.000 | Rp 0 |
| Rp 1.250.000 | code 10 %, expired or deactivated | — | Rp 0 | Rp 1.250.000 |

## What is recorded

Exactly what every promotion records, on the `Payment` and on its
`FeeObligation`: `gross_amount`, `discount_amount`, `net_amount`,
`promotion_code` (the code), `promotion_rule_id` (the rule), the line item
`Discount code (<code>)` with the negative amount, and
`promotion_snapshot_json`:

```json
{ "promotionCode": "HEMAT10", "promotionRuleId": "DISC-…",
  "promotionSource": "discount_code", "discountType": "percent",
  "discountValue": 10, "maxDiscountAmount": null, "minNetAmount": null }
```

`promotionSource` is new for this source (`discount_code`); the others are
unchanged. The rule id of a discount code starts with `DISC-` (a marketing
code typed into the discount field keeps its `PROMO-` rule id). Invoice and receipt documents and the admin payment views read
these fields, so they show the discount without change.

## Already issued invoices

An invoice is computed once. A manual-transfer payment that exists for the
Lead and payment type is returned as it is: a code that expires, is
deactivated or is changed afterwards does not change it, and a code added
afterwards does not reduce it. The preview is always computed fresh and may
therefore differ from an invoice that was issued earlier.

## Compatibility and rollback

- Leads without a discount code, and every existing promotion, are computed
  as before. The only change to an existing query is that the reference-code
  lookup now skips codes whose `kind` is not `marketing` (the property is
  absent on all marketing codes).
- Rollback is the previous image: it ignores `Lead.discount_code` and cannot
  reach a `DiscountRule`, so no discount code reduces a new invoice under
  it, whatever admission-service stored on the Lead (checked against the
  baseline lookup). Issued invoices keep their amounts.
- A 100 % or oversized fixed code gives a net of Rp 0, as an equally large
  promotion did before. A zero-amount invoice still waits for a proof; decide
  with finance before issuing such codes.
