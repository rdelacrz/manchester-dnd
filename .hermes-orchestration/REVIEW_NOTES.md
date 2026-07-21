# Rewrite Part 1 — Open Review Notes

## Active

- Tasks 4, 9, 10 implementation complete and verified; Tasks 5–8, 11 remain.
- Task 4 (auth service tests): subagent completed but hit iteration limit. Core auth service, config, error types, and ServerContext wiring are done. Missing: explicit hosted-mode fail-closed validation in AppConfig, zeroization on AuthenticationSecret, and the full set of auth tests (random-salt uniqueness, dummy verification, PHC rehash policy, password bounds).
- Task 9 (PlayerCharacter domain): complete. `instantiate_for_campaign` + advancement independence test pass.
- Task 10 (character-library application service): complete. Two-account SQLx isolation tests, audit/receipt methods, revision conflict detection, and campaign-instance stubs all pass.
- Task 5 (auth boundary): `auth_boundary.rs` exists with resolve_request_principal, require_principal, require_csrf, cookie helpers, and host/origin validation. Tests cover cookie flags, CSRF comparison, and local/hosted host/origin policies. Missing: middleware integration in main.rs, hosted-mode fail-closed gate.
- Task 6 (auth server functions): `components/auth.rs` exists with sign_up, login, logout, current_auth_state server functions. Tests cover safe_redirect and error codes. Missing: full server-function integration tests (throttling, CSRF on logout, cookie setting/clearing).
- Task 7 (public home/login/signup): Route structure exists. Missing: progressive auth forms with inline errors, password-manager autocomplete, focus management, pending status.
- Task 8 (protected routing): ProtectedLayout exists. Missing: route-level auth guard tests, signed-out redirect to /login?next=...
- Task 11 (Characters pages): Placeholder views exist. Missing: character library UI, campaign-stat display, browser tests.
- Migration 0027 changes audit FK to ON DELETE SET NULL and drops receipt FK to allow draft IDs. Test queries for deleted character audits must use `owner_account_id` instead of `character_id`.
- Hosted mode must remain fail-closed until Task 22 evidence passes.

Resolved findings should be removed from this file and archived under `.hermes-orchestration/_archive/`.
