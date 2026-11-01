## Rulebook check (docs/RUST_RULES_OF_THUMB.md)

The guard (`scripts/check-rulebook.sh`, run in CI before the image build) covers what it can count; this list covers what it cannot.

- [ ] Handlers stay thin; business rules live in services/domain, and domain code imports no infrastructure.
- [ ] Errors are typed; a 5xx body carries a generic message and the detail goes to the log (no secrets or personal data).
- [ ] New configuration is read in the config module, secrets have no default, and gitops sets the value in every environment.
- [ ] Nothing copied that already exists here: an existing helper or client is reused.
- [ ] New behaviour has a test; a bug fix has a regression test.
