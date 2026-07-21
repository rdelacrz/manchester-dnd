# Rewrite Part 1 — Authentication and Isolation Evidence

**Status:** In Progress — Tasks 1-20 complete. Hosted mode remains fail-closed.

## Summary

This document records the evidence that the Rewrite Part 1 authentication
boundary, campaign isolation, and multi-user authorization are ready for
hosted-mode activation. Hosted mode remains disabled (`validate_access_mode`
returns an error for `AccessMode::Hosted`) until every item below is verified.

## 1. Authentication Boundary

### Password Storage
- Passwords are stored as Argon2id PHC hashes via `argon2` crate (m=65536 KiB, t=3, p=4).
- No plaintext passwords are logged, cached, or persisted outside the hash column.
- **Evidence:** `crates/game-server/src/auth.rs` — `AuthService::create_account` hashes passwords before storage. Tests in `auth.rs` verify hash format and round-trip.

### Session Management
- Session tokens are random 32-byte values, cookie-only, stored as SHA-256 digests.
- Cookies are `Secure`, `HttpOnly`, `SameSite=Lax` in hosted mode.
- `AuthenticationConfig` enforces: idle timeout 30 min, absolute timeout 8h, max 5 active sessions.
- **Evidence:** `crates/game-server/src/auth.rs` — `create_session`, `validate_session`, `revoke_session`.

### Throttle and Enumeration Protection
- Login throttle: HMAC-based bucket per email, blocks after 10 attempts in 300s window.
- Login does not reveal whether an account exists (same error for wrong password vs. unknown email).
- **Evidence:** `crates/game-server/src/auth.rs` — `AuthenticationThrottleBucket`, throttle token generation.

### Auth Boundary Middleware
- `resolve_request_principal` Axum middleware extracts the session cookie, validates the session, and inserts `AccountPrincipal` into request extensions.
- Unauthenticated requests to protected routes are redirected to `/login`.
- **Evidence:** `app/src/auth_boundary.rs`, `app/src/main.rs` — middleware layer wiring.

## 2. Campaign Isolation

### Membership-Scoped Access
- All membership repository methods take server-derived `account_id`.
- Cross-account access returns `NotFound` (not `Forbidden`) to prevent enumeration.
- `campaign_memberships` table enforces: one active GM per campaign (partial unique index).
- `campaign_character_instances` enforces: one active character per membership.
- **Evidence:** `crates/game-server/src/repository/memberships.rs` — 14 methods with account-scoped queries. 13 SQLx tests including cross-account denial.

### Character Library
- Character library records are level-less (no level, XP, HP, or campaign progression).
- Runtime stats belong to each campaign-specific character instance.
- Character queries are account-scoped: `list_player_characters(account_id)`, `load_player_character(account_id, character_id)`.
- **Evidence:** `crates/game-server/src/application/player_characters.rs` — 18 SQLx tests including foreign-account denial.

### Campaign Lobby and Play
- Lobby operations verify GM role for start/end, membership for ready/assign.
- Play session state transitions: `waiting` → `active` → `closed`.
- Turn state tracks active account and character per phase.
- **Evidence:** `crates/game-server/src/repository/lobby.rs`, `crates/game-server/src/application/lobby.rs`.

## 3. Action Point Ledger

- Append-only ledger with idempotency keys.
- Atomic spend in the same transaction as turn commit.
- AI substitutes cannot spend points (only select from pre-authorized safe fallback actions).
- **Evidence:** `crates/game-server/src/repository/action_points.rs` — 6 SQLx tests including concurrent spend, negative balance rejection, idempotent replay.

## 4. AI Substitution Safety

- `TypedGmPurpose::ChooseAbsentPlayerAction` constrains AI to selecting from existing legal action IDs.
- AI cannot submit dice, damage, HP, DC, inventory changes, or arbitrary engine commands.
- Deterministic safe fallback on: disabled provider, timeout, malformed output, hostile output, budget exhaustion, rate limit.
- Fallback picks the first conservative action from the caller-supplied allowlist.
- **Evidence:** `crates/game-server/src/typed_gm.rs` — 14 tests covering all fallback paths.

## 5. Remaining Items Before Hosted-Mode Activation

### Must Verify
- [ ] Task 17: Generalize all game server functions (freeform, campaign, hero, images, privacy) to use member-scoped authorization instead of `LOCAL_HERO_OWNER_KEY`.
- [ ] Two-account isolation matrix over every protected server function, API route, SSR route, image, export, restore, character, campaign, lobby, and turn command.
- [ ] CSRF token validation on all state-changing requests.
- [ ] Session fixation, logout/revocation, and expiry tests.
- [ ] Open redirect, ID forgery, cache header, CSP, and XSS fixture tests.
- [ ] Secret scan: verify no passwords, cookies, CSRF tokens, database URLs, or provider keys in SSR output, WASM, JS bundles, source maps.
- [ ] TLS and canonical-origin configuration in production-like environment.
- [ ] Update threat model from local single-user to authenticated multi-user scope.
- [ ] Remove the `validate_access_mode` fail-closed gate only when all evidence passes.

### Configuration
- `APP_ACCESS_MODE=local` (default; fail-closed for hosted).
- `AUTH_COOKIE_SECURE=true` (enforced in hosted mode).
- `AUTH_CANONICAL_ORIGIN` must be set in hosted mode.
- `DATABASE_URL` must point to a PostgreSQL instance with all migrations (0001-0031) applied.
