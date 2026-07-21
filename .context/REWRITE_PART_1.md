# Multi-Page Account and Campaign Rewrite — Part 1 Implementation Plan

> **For Hermes:** Use `subagent-driven-development` to implement this plan task-by-task. Require specification review, security review, and code-quality review before advancing each phase.

**Goal:** Split the current single-page local game into a public introduction, secure account flows, authenticated navigation, account-owned character and campaign pages, a multiplayer lobby, and an authenticated turn-based campaign page with constrained AI substitution for absent players.

**Architecture:** Keep the existing Rust modular monolith: Leptos/Axum in `app/`, deterministic rules in `crates/game-core/`, and authentication, authorization, orchestration, LLM adapters, and PostgreSQL access in `crates/game-server/`. Replace the fixed `local-owner`/`local-campaign` trust boundary with an authenticated `AccountPrincipal`, while retaining a temporary local-mode compatibility principal. Route-level UI guards improve navigation, but every server function and asset/API route must perform server-side object authorization.

**Tech Stack:** Rust 2024, Leptos 0.8 SSR/hydration, Axum 0.8, SQLx 0.8, PostgreSQL, Argon2id PHC password hashes, opaque server-side sessions, Playwright, axe-core.

---

## 1. Current State and Rewrite Constraints

The rewrite must evolve, not replace, the working game path.

- `app/src/app.rs` currently routes `/` to one `Home` component plus three public information pages.
- `app/src/views/home.rs` currently contains the introduction, campaign lifecycle, hero creation, social scene, exploration, encounter, privacy, images, and preferences in one large route.
- `app/src/main.rs` provides Axum/Leptos SSR, CSP nonces, request limits, local Host/Origin checks, and protected image/restore routes.
- `crates/game-server/src/application.rs` and `application/{hero,lifecycle}.rs` assume fixed IDs: `local-owner`, `local-campaign`, and `local-hero`.
- `migrations/0010_campaign_lifecycle.sql` already records `owner_key`, campaign lifecycle, and play sessions, but only for the single local owner.
- `migrations/0004_hero_creation_and_advancement.sql` makes heroes campaign-bound and permits one hero per owner per campaign.
- `app/src/components/freeform.rs` already interprets custom player text only against server-derived legal actions, records generation evidence, and commits mechanics before narration. Preserve this security boundary.
- `crates/game-core/src/ai_turn.rs` already models inert typed AI proposals that cannot mutate authoritative state directly.
- `APP_ACCESS_MODE=hosted` currently fails closed. Do not enable it until authentication, CSRF, object authorization, cross-account tests, TLS/cookie configuration, and protected asset checks all pass.
- Existing local saves and canonical exports must remain readable. Use additive migrations and explicit upcasters; do not rewrite old JSON in place without compatibility fixtures.

## 2. Product Assumptions for Part 1

These assumptions make the plan executable without silently changing game rules.

1. Accounts use a unique normalized email address, a display name, and a password.
2. Successful sign-up creates a session and sends the player to `/characters`.
3. The public home page remains available to everyone and contains introductory/marketing content only.
4. The authenticated side navigation appears only inside protected routes.
5. A character in the character library is account-owned, campaign-independent, and **has no level, XP, HP, or other campaign progression attached to it**. It stores identity and reusable creation choices only. Joining a campaign creates a campaign-bound runtime instance; level, XP, HP, resources, conditions, equipment changes, and advancement belong exclusively to that instance so progression in one campaign cannot corrupt another campaign.
6. Campaign roles are `game_master` and `player`. The campaign creator becomes the game master and owner.
7. The current constrained LLM produces game-master scene text. The human game master controls the lobby/session and may choose scenario controls, but cannot bypass deterministic mechanics. A fully human-authored GM mode is outside Part 1 unless explicitly requested.
8. Missing players may be AI-controlled only after the game master explicitly selects **Start with AI substitutes**. AI substitutes can choose only currently legal actions, cannot spend a human player's custom-action points, and use a deterministic conservative fallback when the LLM is unavailable.
9. Structured actions cost zero custom-action points. A custom prompt is charged only when it is accepted as a valid engine command and the authoritative turn commits. Invalid input, clarification, provider failure, and exact idempotent replay do not charge twice.
10. Password reset, email verification, OAuth, public campaign sharing, spectators, and cross-campaign character progression are deferred. Their absence must be visible in supported-feature documentation.

### Required product decisions before Task 18

Record these as constants/policy data, not scattered literals:

- custom-action point name;
- starting balance;
- whether/how points are earned;
- maximum balance;
- cost per accepted custom action;
- whether a player may recover points after a game-master cancellation;
- maximum campaign party size;
- invitation mechanism for the first release: normalized-email invitation, join code, or both;
- whether the human campaign owner may author GM text in addition to LLM-generated text.

Recommended initial point policy for implementation tests: `3` starting points, `1` point per accepted custom action, no AI spending, no automatic regeneration. Treat this as provisional until approved.

## 3. Target Information Architecture

| Route | Access | Layout | Purpose |
| --- | --- | --- | --- |
| `/` | Public | Public header/footer | Introductory home page only |
| `/login` | Signed-out | Public auth layout | Log in; link to sign-up |
| `/signup` | Signed-out | Public auth layout | Create an account; link to login |
| `/guide` | Public | Public header/footer | Existing setup and feature guide |
| `/privacy-and-safety` | Public | Public header/footer | Existing safety/reporting content |
| `/legal` | Public | Public header/footer | Existing legal/attribution content |
| `/characters` | Authenticated | Side navigation | List only the current account's characters |
| `/characters/new` | Authenticated | Side navigation | Rules-valid character creation wizard |
| `/characters/:character_id` | Authenticated owner | Side navigation | View a level-less owned character and open its campaign list |
| `/characters/:character_id/campaigns/:campaign_id` | Authenticated owner + campaign member | Side navigation | View that character's level and other stats for one campaign instance |
| `/campaigns` | Authenticated | Side navigation | Owned/member campaigns and invitations |
| `/campaigns/new` | Authenticated | Side navigation | Create a campaign |
| `/campaigns/:campaign_id/lobby` | Authenticated member | Side navigation | Select character, ready up, wait, or start |
| `/campaigns/:campaign_id/play` | Authenticated member | Side navigation | Run the turn-based campaign |

### Public header behavior

- Brand links to `/`.
- Signed-out state shows **Log in** at the top and a secondary **Sign up** action.
- Signed-in state shows **Open game** and an account/logout control, not the side navigation.
- A safe relative `next` path may return a player to a protected page after login. Reject absolute URLs, scheme-relative URLs, and unknown protected routes.

### Authenticated side navigation

- Home
- Characters
- Campaigns
- Current campaign, only when one is active for the account
- Guide
- Privacy & safety
- Legal
- Account display name and Logout

Use a semantic `<nav aria-label="Player navigation">`. On narrow screens, use an accessible disclosure/drawer with focus return, Escape handling, visible current-route state, and no layout overflow. Do not render this component on `/`, `/login`, `/signup`, or public information routes.

### Character library and campaign-stat display

- Character cards and `/characters/:character_id` show identity and reusable choices, but never a global level, XP, current HP, or campaign-derived stat total.
- Each owned character provides a **Campaigns (N)** control. Implement it as an accessible modal dialog on wide screens and an inline disclosure/full-width sheet on narrow screens; if JavaScript is unavailable, the control links to the equivalent server-rendered campaign list on the character detail page.
- The display lists every currently authorized campaign instance derived from that library character. Each row shows campaign title, membership/status, and a link; the level is not treated as a property of the library card.
- Clicking a campaign link opens `/characters/:character_id/campaigns/:campaign_id`, which displays that instance's current level, XP/progression, HP/max HP, ability/derived statistics, class features, resources, conditions, equipment, spells, and last-updated revision as applicable to the rules engine.
- The route and backing query require both ownership of the source character and current authorization for the campaign/runtime instance. A forged character/campaign pair returns the same safe not-found response as a missing instance.
- One library character can therefore appear at different levels and with different runtime stats in different campaigns without ambiguity.

## 4. Security Design

### 4.1 Password storage

Passwords must be **hashed, not encrypted**. Reversible password encryption is not acceptable.

- Use Argon2id through the Rust `argon2`/`password-hash` APIs.
- Generate a unique cryptographically random salt for every password.
- Store one PHC string containing algorithm, version, parameters, salt, and digest. Do not store plaintext passwords or a separate recoverable password field.
- Benchmark memory/time/parallelism parameters on production-class hardware and target an intentionally expensive login hash without exhausting server concurrency. Enforce a reviewed minimum and store parameters in each PHC string.
- Verify with the library's constant-time implementation.
- Perform a dummy Argon2id verification when the normalized email does not exist to reduce account enumeration through timing.
- Rehash on successful login when stored parameters are below the current policy.
- Permit password-manager paste and long passphrases. Normalize only the account email; do not silently Unicode-normalize or trim the password.
- Initial policy: 15–128 Unicode scalar values, reject known-compromised/common test fixtures, and never impose composition rules such as “one symbol.”
- Keep an optional application pepper outside PostgreSQL only if an operational rotation/recovery procedure is implemented and tested. Do not add an unmanaged pepper that can permanently lock out all accounts.

### 4.2 Sessions and cookies

- Generate at least 256 bits of CSPRNG session-token entropy.
- Send the raw token only in a cookie. Store only `SHA-256(token)` in `account_sessions`; high token entropy makes a fast lookup digest appropriate.
- Cookie: `HttpOnly`, `SameSite=Lax`, `Path=/`, host-only, no `Domain`, and `Secure` in hosted mode.
- Hosted startup requires HTTPS-aware configuration and refuses insecure cookies. Loopback development may use an explicit non-secure development cookie profile.
- Rotate the session on login and password change. Revoke on logout. Enforce idle and absolute expiry plus a bounded number of active sessions per account.
- Never place the session token in HTML, Leptos serialized state, logs, URLs, local storage, or WASM constants.

### 4.3 CSRF and request boundaries

- Preserve strict methods, content types, body limits, Origin checks, CSP, clickjacking denial, `nosniff`, no-store, and safe transport errors.
- Add a per-session CSRF secret. Store only its digest. Render/send the raw CSRF token separately from the HttpOnly cookie and require it on every authenticated mutation, including logout, campaign restore, and image-generation controls.
- Compare CSRF values in constant time.
- Treat `SameSite` and Origin checks as defense in depth, not as replacements for CSRF tokens.
- Split the current `valid_local_host` logic by access mode. Local mode remains loopback-only; hosted mode accepts only configured canonical HTTPS origins/hosts.

### 4.4 Authorization

Create one server-side principal type:

```rust
pub struct AccountPrincipal {
    pub account_id: String,
    pub session_id: String,
}
```

Every protected server function must call a shared helper that:

1. validates the session cookie and expiry;
2. validates CSRF for mutations;
3. obtains the current `account_id` from server state, never a browser-provided owner ID;
4. authorizes the requested character/campaign/play-session/artifact through an ownership or membership query;
5. returns `not_found` for inaccessible opaque object IDs where distinguishing forbidden from missing would leak existence.

UI route guards are not authorization. Repository methods should prefer signatures such as:

```rust
load_owned_character(account_id, character_id)
load_member_campaign(account_id, campaign_id)
commit_member_action(account_id, campaign_id, character_id, command)
```

Do not expose unscoped `load_character(id)` or `load_campaign(id)` at browser-facing application boundaries.

### 4.5 Abuse resistance and logs

- Add account/IP-keyed login and sign-up throttling. Use HMAC-digested throttle keys so raw normalized emails and IP addresses are not retained in operational buckets.
- Apply progressive delays, not permanent account lockouts that permit denial-of-service against a known email.
- Return the same login error for unknown account, disabled account, and wrong password.
- Do not log email, password, cookie, CSRF token, custom prompt text, or narration body.
- Trace only opaque account/campaign IDs where operationally necessary and keep cardinality bounded.

## 5. Target Data Model

Current highest migration is `0024`; use the following additive sequence.

### `migrations/0025_accounts_and_sessions.sql`

Create:

- `accounts`
  - `id TEXT PRIMARY KEY` using opaque `account:<uuid>` IDs;
  - `normalized_email TEXT UNIQUE`;
  - `display_name TEXT NOT NULL`;
  - `password_phc TEXT`;
  - `login_enabled BOOLEAN NOT NULL DEFAULT TRUE`;
  - `password_changed_at`, `created_at`, `updated_at`;
  - bounded-length and normalized-email constraints;
  - a check requiring normalized email and a PHC hash when `login_enabled = true`, while permitting both to be absent for the non-login local compatibility principal.
- `account_sessions`
  - `id TEXT PRIMARY KEY`;
  - `account_id` FK `accounts(id) ON DELETE CASCADE`;
  - unique `token_digest` and `csrf_digest` with strict digest checks;
  - `created_at`, `last_seen_at`, `idle_expires_at`, `absolute_expires_at`, `revoked_at`;
  - indexes for active lookup and expiry cleanup.
- `auth_throttle_buckets`
  - HMAC-digested key, action kind, window, attempt count, and `blocked_until`;
  - no raw email/IP.
- `authentication_audits`
  - opaque account ID when known, event kind, success/failure class, correlation ID, timestamp;
  - no email, secrets, or request bodies.

Insert an internal `account:local` row with `login_enabled = false` for existing local-mode data. It is an internal compatibility principal, not a hosted login account.

### `migrations/0026_player_character_library.sql`

Create account-owned, campaign-independent storage:

- `player_character_drafts` keyed by draft ID and `owner_account_id`;
- `player_characters` keyed by character ID and `owner_account_id`;
- `player_character_audits` and idempotency receipts;
- payload/schema/revision/timestamp/retention constraints matching current hero rigor;
- index `(owner_account_id, updated_at DESC, id)`.

Do not repurpose campaign runtime rows as library records. Introduce a new domain document that reuses validated identity and `HeroChoices`, but has no `campaign_id`, level, XP, current/max HP, resources, conditions, or mutable campaign equipment. Sheet derivation that depends on level or runtime state occurs only after a campaign instance is created.

### `migrations/0027_campaign_memberships.sql`

Alter/create:

- add `owner_account_id TEXT REFERENCES accounts(id)` to `campaign_sessions`;
- backfill from `owner_key`, mapping existing local rows to `account:local`;
- retain `owner_key` during one compatibility release, then remove it in a later cleanup migration;
- `campaign_memberships`
  - `(campaign_session_id, account_id)` primary key;
  - role `game_master|player`;
  - state `invited|active|left|removed`;
  - inviter, accepted/left timestamps;
  - exactly one owning game-master membership;
- `campaign_invitations`
  - opaque ID, campaign ID, invitee account/email digest or join-code digest, expiry, accepted/revoked timestamps;
- `campaign_character_instances`
  - campaign ID, account ID, source `player_character_id`, runtime `hero_character_id`;
  - unique active character per membership;
  - immutable source-choice snapshot and campaign-compatible content pins;
  - campaign-specific level, XP, HP, resources, conditions, equipment, spell state, and derived sheet remain in the linked runtime hero document, not in `player_characters`;
  - index `(account_id, player_character_id, campaign_session_id)` for the owned character's campaign display.

All campaign list/load queries join membership and scope by current account. A player sees campaigns they own or have accepted membership in, never all campaigns.

### `migrations/0028_campaign_lobbies_and_turns.sql`

Evolve the current `campaign_play_sessions` model:

- states `waiting|active|closed`;
- start policy `wait_for_all|start_with_ai_substitutes`;
- game-master account ID;
- expected membership revision/snapshot;
- active turn revision.

Create:

- `campaign_play_session_participants`
  - play-session ID, account ID, runtime character ID;
  - state `not_ready|ready|human_active|ai_substitute|left`;
  - presence/ready timestamps and handoff revision;
- `campaign_turn_states`
  - one current state per active play session;
  - phase `game_master_generation|player_action|resolving|completed`;
  - active account/character, round, turn number, revision, and bounded JSON payload;
- append-only turn-control audits for lobby start, AI substitution, human handoff, and turn-phase transitions.

Do not infer readiness from an open WebSocket or browser tab. Readiness is an explicit durable player action. Presence may be advisory, but start eligibility is calculated from membership plus durable ready state.

### `migrations/0029_custom_action_points.sql`

Create an append-only ledger:

- `custom_action_point_ledger`
  - account, campaign, runtime character, play session, turn, amount, reason, idempotency key, created time;
  - reasons `initial_grant|earned|custom_action_spent|administrative_refund`;
  - unique spend identity preventing duplicate charge;
- optional materialized balance row locked with `SELECT ... FOR UPDATE`, with a constraint that balance never becomes negative;
- receipt linkage from accepted custom intent to the committed authoritative turn.

The authoritative game transaction must commit the mechanical turn, point spend, turn audit, and idempotency receipt together. Do not decrement points in browser state or in a separate best-effort transaction.

## 6. Turn and Lobby State Machines

### Lobby

```text
campaign selected
  -> waiting play session created
  -> each active member selects an owned compatible character
  -> each member marks ready
  -> GM chooses:
       wait_for_all: remain waiting until all active members are ready
       start_with_ai_substitutes: ready members become human_active;
                                  missing members become ai_substitute
  -> active play session + initial GM turn committed atomically
```

Rules:

- Only a game master may start or end a play session.
- A player can ready only their own membership and owned character.
- The same account/character cannot occupy two slots in one campaign.
- Joining after start schedules human control at the next safe turn boundary; it never interrupts an in-flight generation or transaction.
- Member removal, campaign archive, or session end closes/substitutes slots through audited transitions.

### Campaign play

```text
game_master_generation
  -> constrained LLM scene/narration proposal
  -> validate facts/policy and save presentation or deterministic fallback
  -> player_action(active participant)
       structured legal action: cost 0
       custom intent: reserve/validate -> legal engine command -> spend 1 atomically
       absent participant: AI proposes one legal action, cost 0
  -> deterministic mechanics commit
  -> optional narration presentation
  -> next participant or next game_master_generation
```

The browser receives:

- public scene text and recent turn history;
- current phase/actor;
- legal actions only when the current account owns the active human slot;
- custom-action balance and field only when custom actions are permitted;
- lobby/member status safe for campaign members;
- no hidden GM prompt, provider credential, raw private source, enemy hidden state, or another account's private character library data.

### Missing-player AI

Add `TypedGmPurpose::ChooseAbsentPlayerAction` and a purpose-specific input/output schema.

- Input contains the missing runtime character's public sheet summary, public scene facts, and legal action/target IDs.
- Output must be an existing legal action ID plus optional target ID.
- Validate through `ProposalAcceptanceContext`; the model cannot submit dice, damage, HP, DC, inventory changes, points, or arbitrary engine commands.
- For irreversible/high-stakes actions, use a conservative allowlist or pause for the game master.
- On timeout, invalid schema, policy rejection, or budget exhaustion, choose a deterministic safe fallback such as defend, move to safety, or end turn. The exact fallback must be engine-authored and tested.
- Record `actor = ai_substitute`, original account/character slot, model/config evidence or fallback policy ID, and handoff state in audits.

## 7. Implementation Tasks

> **Progress checklist:** `[x]` means fully implemented and evidenced in the current repository. Partial, placeholder, unverified, and not-started work remains `[ ]`. Test-command boxes require a passing run; commit boxes require a matching Git commit.

Each code task follows: failing test, verify failure, minimal implementation, verify pass, then commit. Run PostgreSQL tests with `DATABASE_URL` set to the isolated-test-capable development role.

### Task 1: Freeze existing behavior before route extraction

**Objective:** Preserve the working local campaign while files and routes move.

**Files:**
- Modify: `tests/browser/slice0.spec.ts`
- Modify: `tests/browser/release-journey.spec.ts`
- Create: `tests/browser/navigation-regression.spec.ts`

**Steps:**
- [x] Add assertions identifying the current intro, character, campaign, and gameplay regions.
- [x] Add a browser fixture that can later point the same journey at protected routes.
- [ ] Run `npm run test:browser`; expected: current suite passes before rewrite.
- [ ] Commit: `test: freeze pre-rewrite navigation behavior`.

### Task 2: Extract public and authenticated layouts without enabling auth

**Objective:** Make route splitting possible while retaining current styling and accessibility.

**Files:**
- Create: `app/src/components/layout.rs`
- Create: `app/src/components/public_header.rs`
- Create: `app/src/components/side_navigation.rs`
- Modify: `app/src/components/mod.rs`
- Modify: `app/src/app.rs`
- Modify: `style/main.css`

**Steps:**
- [x] Add component tests/SSR assertions that public layout has no side navigation and authenticated layout does.
- [x] Extract brand, header, footer, skip link target, and responsive shell from `Home`.
- [x] Preserve the current design tokens and focus/touch-target behavior.
- [x] Run `cargo test --locked -p manchester-dnd-app` and the navigation browser test.
- [ ] Commit: `refactor: extract public and authenticated layouts`.
  - Split-staged Task 2 commit created as `0a4b88b refactor: extract public and authenticated layouts` after controller approval. Checkbox intentionally remains open because mixed later-task files (`app/src/app.rs`, `app/src/components/mod.rs`, `style/main.css`) were explicitly left for their own task commits.

### Task 3: Add account/session schema and repository

**Objective:** Persist accounts, password hashes, sessions, throttles, and authentication audits.

**Files:**
- Create: `migrations/0025_accounts_and_sessions.sql`
- Create: `crates/game-server/src/auth.rs`
- Create: `crates/game-server/src/repository/auth.rs`
- Modify: `crates/game-server/src/repository.rs`
- Modify: `crates/game-server/src/lib.rs`
- Modify: `crates/game-server/src/error.rs`
- Modify: `Cargo.toml`
- Modify: `crates/game-server/Cargo.toml`

**Steps:**
- [x] Write `#[sqlx::test(migrator = "crate::repository::MIGRATOR")]` tests for uniqueness, cascade revocation, expiry lookup, and local-principal backfill.
- [x] Add strict DTOs and typed `AuthenticationError`; do not collapse repository/hash/session failures into strings.
- [x] Implement scoped account/session repository methods using parameterized SQL.
- [x] Run targeted tests; expected: migration and repository tests pass.
- [ ] Commit: `feat: add durable account and session storage`.

### Task 4: Implement Argon2id sign-up and login services

**Objective:** Create and verify accounts without exposing password or account-existence data.

**Files:**
- Modify: `crates/game-server/src/auth.rs`
- Modify: `crates/game-server/src/config.rs`
- Modify: `crates/game-server/src/context.rs`
- Modify: `.env.example`

**Steps:**
- [ ] Write tests for random-salt uniqueness, correct verification, wrong-password rejection, unknown-account dummy verification, PHC rehash policy, password bounds, and secret-redacted errors.
- [x] Implement `AuthService::{sign_up, login, logout, authenticate, cleanup_expired_sessions}`.
- [ ] Add explicit session lifetime, cookie security, canonical origin, and optional pepper configuration with fail-closed hosted validation.
- [ ] Ensure password values use secret wrappers/zeroization where practical and never implement `Debug` with contents.
- [x] Run `cargo test --locked -p manchester-dnd-server auth`.
- [ ] Commit: `feat: implement secure account authentication`.

### Task 5: Add Axum session extraction, CSRF, and hosted boundary configuration

**Objective:** Produce a trusted request principal before protected application calls.

**Files:**
- Modify: `app/src/main.rs`
- Create: `app/src/auth_boundary.rs`
- Modify: `app/src/lib.rs`
- Modify: `app/Cargo.toml`
- Modify: `crates/game-server/src/config.rs`

**Steps:**
- [ ] Write middleware tests for missing/expired/revoked tokens, cookie flags, session rotation, CSRF mismatch, local-host behavior, and configured hosted origin behavior.
- [x] Parse the host-only cookie and resolve it through `AuthService`; attach `Option<AccountPrincipal>` to request extensions.
- [ ] Add `require_principal()` and `require_csrf()` helpers used by server functions and dedicated API routes.
- [x] Replace unconditional `valid_local_host` logic with access-mode-specific validation.
- [ ] Keep `APP_ACCESS_MODE=hosted` fail-closed; add a temporary test-only gate until all release conditions in Task 22 pass.
- [x] Run app and server unit tests.
- [ ] Commit: `feat: add authenticated HTTP and CSRF boundary`.

### Task 6: Add login, sign-up, logout, and current-account server functions

**Objective:** Expose bounded public auth operations and a safe session-state query.

**Files:**
- Create: `app/src/components/auth.rs`
- Modify: `app/src/components/mod.rs`
- Modify: `app/src/main.rs`

**Steps:**
- [x] Add strict `SignUpInput`, `LoginInput`, `AuthStateView`, and public error envelopes with unknown fields denied.
- [ ] Test generic login errors, throttling, safe `next` validation, cookie setting/clearing, CSRF enforcement on logout, and no secret fields in serialized responses.
- [x] Implement server functions that call `AuthService`; do not query accounts directly from UI code.
- [ ] Run targeted server-function tests.
- [ ] Commit: `feat: expose bounded account session functions`.

### Task 7: Split the public home, login, and sign-up pages

**Objective:** Make `/` introductory only and expose accessible auth routes.

**Files:**
- Replace: `app/src/views/home.rs` with the introduction-only view
- Create: `app/src/views/login.rs`
- Create: `app/src/views/signup.rs`
- Modify: `app/src/views/mod.rs`
- Modify: `app/src/app.rs`
- Modify: `style/main.css`
- Create: `tests/browser/auth-navigation.spec.ts`

**Steps:**
- [x] Write browser tests proving `/` has intro content but no campaign, character, encounter, or side navigation UI.
- [ ] Test signed-out top **Log in** link and login-to-sign-up link.
- [ ] Implement progressive, labeled auth forms with inline non-enumerating errors, password-manager-compatible autocomplete, focus management, and pending status.
- [ ] On success, redirect only to a validated relative `next` or `/characters`.
- [ ] Run SSR-without-WASM, hydration, keyboard, responsive, and axe checks.
- [ ] Commit: `feat: add public home and account pages`.

### Task 8: Add protected routing and authenticated side navigation

**Objective:** Prevent signed-out navigation into application pages and hide the side navigation unless authenticated.

**Files:**
- Create: `app/src/components/protected_layout.rs`
- Modify: `app/src/app.rs`
- Modify: `app/src/components/side_navigation.rs`
- Modify: `tests/browser/auth-navigation.spec.ts`

**Steps:**
- [ ] Test signed-out requests to every protected route redirect to `/login?next=...` without rendering private data.
- [ ] Test signed-in public home still has no side nav; signed-in protected routes do.
- [ ] Implement route-level auth state loading and the protected layout.
- [ ] Keep server-side authorization mandatory even if a client bypasses the route guard.
- [ ] Run auth-navigation browser tests.
- [ ] Commit: `feat: protect application routes and navigation`.

### Task 9: Introduce account-owned character-library domain types

**Objective:** Decouple character creation choices from campaign runtime state without weakening D&D validation.

**Files:**
- Create: `crates/game-core/src/player_character.rs`
- Modify: `crates/game-core/src/lib.rs`
- Refactor: `crates/game-core/src/hero.rs`

**Steps:**
- [x] Write tests proving `PlayerCharacter` validates owner, identity, reusable choices, schema, and revision without a campaign ID or any level/XP/HP/runtime fields.
- [x] Add negative serialization/domain tests proving library characters cannot acquire campaign progression fields.
- [ ] Extract/reuse the existing creation-state transitions without copying rules; defer level-dependent sheet derivation to campaign instantiation.
- [ ] Add a tested conversion that instantiates a campaign-bound level-one (or campaign-policy starting-level) `HeroCharacter` from an owned library character plus campaign-compatible pins.
- [ ] Prove two campaign instances from the same library character can advance independently to different levels/stats.
- [x] Keep old `HeroCharacter` schema deserialization compatible.
- [x] Run `cargo test --locked -p manchester-dnd-core`.
- [ ] Commit: `feat: add level-less player character domain`.

### Task 10: Add character-library persistence and application services

**Objective:** List, create, load, and mutate only characters owned by the current account.

**Files:**
- Create: `migrations/0026_player_character_library.sql`
- Create: `crates/game-server/src/repository/player_characters.rs`
- Create: `crates/game-server/src/application/player_characters.rs`
- Modify: `crates/game-server/src/repository.rs`
- Modify: `crates/game-server/src/application.rs`
- Modify: `crates/game-server/src/error.rs`

**Steps:**
- [ ] Write two-account SQLx tests: account A cannot list/load/mutate account B's character or draft, even with a guessed ID.
- [ ] Add draft retention, optimistic revisions, immutable audits, and idempotency receipts.
- [ ] Parameterize service methods with server-derived `account_id`.
- [ ] Add `list_authorized_campaign_instances(account_id, player_character_id)` and `load_authorized_campaign_character_stats(account_id, player_character_id, campaign_id)`; both must verify the source ownership, campaign membership, and exact source-to-runtime mapping.
- [ ] Test mismatched character/campaign pairs, foreign campaigns, removed memberships, and two independently advanced instances of the same source character.
- [ ] Return the same safe `character_not_found`/instance-not-found result for absent and foreign IDs.
- [ ] Run targeted PostgreSQL and application tests.
- [ ] Commit: `feat: add isolated character library and campaign-stat queries`.

### Task 11: Build Characters pages

**Objective:** Let players create and view their own rules-valid characters.

**Files:**
- Create: `app/src/views/characters.rs`
- Create: `app/src/views/character_new.rs`
- Create: `app/src/views/character_detail.rs`
- Create: `app/src/views/character_campaign_stats.rs`
- Create: `app/src/components/character_library.rs`
- Create: `app/src/components/character_campaigns.rs`
- Refactor: `app/src/components/hero.rs`
- Modify: `app/src/app.rs`
- Create: `tests/browser/character-isolation.spec.ts`

**Steps:**
- [ ] Move the existing wizard UI behind account-scoped server functions and remove its dependency on a fixed local campaign.
- [ ] Show empty, loading, draft-resume, created-character, and inaccessible-ID states; do not show level, XP, HP, or other campaign progression on the character card or base detail.
- [ ] Add the progressive-enhancement **Campaigns (N)** control and accessible dialog/disclosure listing authorized campaign instances.
- [ ] Add the campaign-instance stats route and display current level, XP/progression, HP, derived stats, features, resources, conditions, equipment, spells, and revision from the selected runtime hero.
- [ ] Test the same character appearing in two campaigns at different levels and verify each campaign link shows only that instance's stats.
- [ ] Preserve server validation after every creation step and atomic final commit.
- [ ] Use two browser contexts to prove listing/detail/stat isolation, mismatched pair rejection, and ID-forgery rejection.
- [ ] Run axe, keyboard, hydration, narrow-screen dialog/disclosure, no-JavaScript fallback, and character-isolation tests.
- [ ] Commit: `feat: add level-less character library and campaign stats display`.

### Task 12: Parameterize campaign ownership and lifecycle services

**Objective:** Replace fixed campaign/owner IDs with account-scoped campaign commands.

**Files:**
- Create: `migrations/0027_campaign_memberships.sql`
- Refactor: `crates/game-server/src/application/lifecycle.rs`
- Refactor: `crates/game-server/src/repository/lifecycle.rs`
- Modify: `crates/game-server/src/application.rs`
- Modify: `crates/game-server/src/repository.rs`
- Modify: `crates/game-server/src/error.rs`

**Steps:**
- [ ] Write migration tests for local backfill and membership constraints.
- [ ] Write two-account tests for list/load/history/export/archive/delete/restore and guessed IDs.
- [ ] Replace `list_local_campaigns`, `create_local_campaign`, and related methods with account/campaign-parameterized equivalents; retain thin local-mode wrappers temporarily.
- [ ] Generate campaign IDs server-side and create owner membership atomically with the campaign.
- [ ] Scope canonical restore and image delivery to the authenticated account/member.
- [ ] Run lifecycle, recovery, export, and asset authorization tests.
- [ ] Commit: `feat: add account-scoped campaign ownership`.

### Task 13: Add campaign invitations, membership, and character assignment

**Objective:** Establish the durable party used by the lobby and turn engine.

**Files:**
- Create: `crates/game-server/src/repository/memberships.rs`
- Create: `crates/game-server/src/application/memberships.rs`
- Modify: `crates/game-server/src/application.rs`
- Modify: `crates/game-server/src/repository.rs`
- Modify: `crates/game-server/src/error.rs`

**Steps:**
- [ ] Test game-master-only invitations, expiration/revocation, acceptance, member removal, and party-size limits.
- [ ] Test that a player may select only their own library character.
- [ ] Test content/rules compatibility when creating a campaign-bound runtime character instance.
- [ ] Initialize level, XP, HP, derived stats, and runtime resources on the campaign instance only; do not write them back to the source library character.
- [ ] Test no duplicate active slot and no cross-account character assignment.
- [ ] Test one source character can join multiple campaigns and advance independently according to each campaign's policy.
- [ ] Commit membership and runtime character creation in one transaction.
- [ ] Run targeted SQLx/application tests.
- [ ] Commit: `feat: add campaign membership and isolated character instances`.

### Task 14: Build Campaigns list/create pages

**Objective:** Give authenticated players a dedicated campaign library.

**Files:**
- Create: `app/src/views/campaigns.rs`
- Create: `app/src/views/campaign_new.rs`
- Create: `app/src/components/campaign_library.rs`
- Refactor: `app/src/components/lifecycle.rs`
- Modify: `app/src/app.rs`
- Create: `tests/browser/campaign-isolation.spec.ts`

**Steps:**
- [ ] Move campaign list/create/archive/export/history controls out of `Home`.
- [ ] Render owned campaigns, accepted memberships, and pending invitations separately.
- [ ] Add create form with bounded title and supported policy/theme selections.
- [ ] Route Resume to `/campaigns/:id/lobby` or `/campaigns/:id/play` based on current play state.
- [ ] Test two-account isolation, invitation acceptance, responsive navigation, and accessibility.
- [ ] Commit: `feat: add authenticated campaign pages`.

### Task 15: Implement durable lobby transitions

**Objective:** Support waiting for all players or starting with explicit AI substitutes.

**Files:**
- Create: `migrations/0028_campaign_lobbies_and_turns.sql`
- Create: `crates/game-core/src/campaign_turn.rs`
- Create: `crates/game-server/src/repository/lobby.rs`
- Create: `crates/game-server/src/application/lobby.rs`
- Modify: `crates/game-core/src/lib.rs`
- Modify: `crates/game-server/src/application.rs`
- Modify: `crates/game-server/src/repository.rs`

**Steps:**
- [ ] Write pure state-machine tests for ready/unready, wait-for-all denial, AI-substitute start, duplicate start replay, archive/end, and next-boundary human handoff.
- [ ] Write SQLx concurrency tests proving two start requests create one active play session.
- [ ] Implement explicit membership-revision checks and idempotency receipts.
- [ ] Persist participant control mode and turn-control audits atomically.
- [ ] Run core and PostgreSQL lobby tests.
- [ ] Commit: `feat: add durable multiplayer campaign lobby`.

### Task 16: Build the Campaign Lobby page

**Objective:** Let members select characters, ready up, and start according to policy.

**Files:**
- Create: `app/src/views/campaign_lobby.rs`
- Create: `app/src/components/campaign_lobby.rs`
- Modify: `app/src/app.rs`
- Create: `tests/browser/campaign-lobby.spec.ts`

**Steps:**
- [ ] Test GM/player role differences, character ownership, ready state, wait-for-all, and start-with-AI confirmation.
- [ ] Display durable readiness separately from advisory online presence.
- [ ] Require an explicit confirmation listing players who will be AI-controlled.
- [ ] On successful start, route all members to `/campaigns/:id/play`.
- [ ] Run two-context browser tests plus axe/keyboard checks.
- [ ] Commit: `feat: add multiplayer campaign lobby page`.

### Task 17: Generalize authenticated campaign loading and command authorization

**Objective:** Make every existing exploration, encounter, narration, privacy, image, and lifecycle call member-scoped.

**Files:**
- Refactor: `app/src/components/campaign.rs`
- Refactor: `app/src/components/hero.rs`
- Refactor: `app/src/components/freeform.rs`
- Refactor: `app/src/components/images.rs`
- Refactor: `app/src/components/privacy.rs`
- Refactor: `crates/game-server/src/application.rs`
- Modify: `crates/game-server/src/scene_images.rs`
- Modify: `app/src/main.rs`

**Steps:**
- [ ] Add forged campaign/character/play-session/artifact tests to every server function and dedicated route.
- [ ] Replace `load_local_campaign()` with `load_member_campaign(principal, campaign_id)` and equivalents.
- [ ] Derive active character/control rights from membership and turn state, not from browser IDs.
- [ ] Preserve expected revisions, idempotency, mechanics-first commits, and safe errors.
- [ ] Run the full Rust test suite before UI relocation.
- [ ] Commit: `refactor: authorize all game operations by membership`.

### Task 18: Add custom-action point policy and atomic ledger

**Objective:** Charge accepted custom prompts safely while structured actions remain free.

**Files:**
- Create: `migrations/0029_custom_action_points.sql`
- Create: `crates/game-core/src/action_points.rs`
- Create: `crates/game-server/src/repository/action_points.rs`
- Create: `crates/game-server/src/application/action_points.rs`
- Modify: `app/src/components/freeform.rs`
- Modify: `crates/game-server/src/application.rs`

**Steps:**
- [ ] Confirm and encode the product-policy values listed in Section 2.
- [ ] Write pure tests for grant/spend/refund policy and bounds.
- [ ] Write SQLx tests for concurrent last-point spending, negative-balance rejection, exact replay, changed-payload idempotency conflict, provider failure, clarification, and successful atomic spend+turn commit.
- [ ] Show balance and cost before submission; disable custom input when balance is insufficient.
- [ ] Never allow an AI substitute to spend points.
- [ ] Run targeted and full persistence tests.
- [ ] Commit: `feat: add atomic custom-action point spending`.

### Task 19: Add AI substitution for absent-player turns

**Objective:** Let the LLM play an absent member without granting mechanical authority.

**Files:**
- Modify: `crates/game-core/src/ai_turn.rs`
- Modify: `crates/game-server/src/typed_gm.rs`
- Modify: `crates/game-server/src/gm.rs`
- Create: `crates/game-server/src/application/ai_substitute.rs`
- Modify: `crates/game-server/src/application.rs`

**Steps:**
- [ ] Add strict purpose-specific proposal tests and hostile output fixtures.
- [ ] Prove unknown action/target IDs, point spending, forged mechanics, and stale revisions are rejected.
- [ ] Add deterministic fallback tests for disabled provider, timeout, malformed output, exhausted budget, and no safe action.
- [ ] Commit the chosen legal action through the same authoritative command path as a human structured action.
- [ ] Record control origin and generation/fallback evidence without prompt text.
- [ ] Run typed-GM, rules, application, and generation tests.
- [ ] Commit: `feat: add constrained absent-player AI turns`.

### Task 20: Build the running Campaign Play page

**Objective:** Move actual play to a dedicated authenticated turn-based route.

**Files:**
- Create: `app/src/views/campaign_play.rs`
- Create: `app/src/components/game_master_turn.rs`
- Create: `app/src/components/player_turn.rs`
- Create: `app/src/components/turn_history.rs`
- Refactor: relevant gameplay sections from `app/src/views/home.rs`
- Modify: `app/src/app.rs`
- Create: `tests/browser/campaign-play.spec.ts`

**Steps:**
- [ ] Render current GM scene, phase, active actor, party/control status, recent history, legal actions, and custom input/balance.
- [ ] Show action controls only to the current human actor; others see an accessible waiting status.
- [ ] Show clear `AI controlling this turn` and next-boundary handoff states.
- [ ] Preserve interrupted-request recovery, revision-conflict reload, provider degradation, narration versions, privacy controls, and scene images.
- [ ] Test GM turn -> human action -> absent-player AI action -> human handoff -> next GM turn across multiple browser contexts.
- [ ] Run hydration, no-WASM read-only usefulness, keyboard, reduced-motion, mobile, and axe tests.
- [ ] Commit: `feat: add authenticated campaign play page`.

### Task 21: Migrate existing browser journeys and remove the monolithic game home

**Objective:** Make all tests use real routes and ensure `/` remains introductory.

**Files:**
- Modify: `tests/browser/support/hero-fixture.ts`
- Create: `tests/browser/support/account-fixture.ts`
- Modify: all `tests/browser/*.spec.ts` that currently navigate to `/`
- Modify: Playwright server harnesses/configs under `tests/browser/support/` and `tests/browser/config/`
- Remove migrated gameplay code from `app/src/views/home.rs`

**Steps:**
- [ ] Seed/login deterministic test accounts through supported test setup, never production backdoors.
- [ ] Update hero journeys to `/characters/new`, lifecycle journeys to `/campaigns`, and game journeys to `/campaigns/:id/play`.
- [ ] Keep one local-mode compatibility journey until local migration support is intentionally retired.
- [ ] Run every browser script in `package.json`.
- [ ] Commit: `test: migrate browser journeys to authenticated routes`.

### Task 22: Security and hosted-mode release gate

**Objective:** Enable hosted mode only after the new trust boundary is proven.

**Files:**
- Modify: `crates/game-server/src/config.rs`
- Modify: `app/src/main.rs`
- Modify: `docs/security/threat-model.md`
- Modify: `docs/planning/02-architecture.md`
- Modify: `docs/planning/09-quality-observability-security.md`
- Modify: `README.md`
- Modify: `.env.example`
- Create: `docs/evidence/rewrite-part-1-auth-and-isolation.md`

**Steps:**
- [ ] Run a two-account matrix over every protected server function, API route, SSR route, image, export, restore, character, campaign, lobby, and turn command.
- [ ] Test CSRF, session fixation, cookie flags, logout/revocation, expiry, account enumeration timing tolerance, throttle behavior, open redirects, ID forgery, cache headers, CSP, XSS fixtures, body limits, and log redaction canaries.
- [ ] Scan SSR/WASM/JS/source maps for password, cookie, CSRF, database, and provider canaries.
- [ ] Exercise TLS/canonical-origin and secret-managed configuration in a production-like environment.
- [ ] Update the threat model from local single-user to authenticated multi-user scope.
- [ ] Remove the unconditional hosted-mode startup denial only when all evidence passes; otherwise keep fail-closed behavior.
- [ ] Commit: `security: enable hosted mode after auth isolation gate`.

### Task 23: Compatibility, operations, and final verification

**Objective:** Prove old data survives and new sessions/campaigns recover safely.

**Files:**
- Add/update fixtures under existing server/repository tests
- Modify: `docs/operations/release-operations.md`
- Modify: `docs/operations/database-recovery.md`
- Modify: `docs/CHECKLIST.md`

**Steps:**
- [ ] Load pre-rewrite campaign/hero/export fixtures after all migrations.
- [ ] Test local-principal ownership backfill and an explicit operator claim/migration path if old local data must be attached to a real account.
- [ ] Backup and restore accounts, sessions, memberships, character library, campaigns, lobby, turn state, audits, and point ledger; verify canonical state hashes.
- [ ] Confirm sessions may be intentionally excluded/revoked after disaster recovery and document expected re-login behavior.
- [ ] Run the full verification suite below.
- [ ] Commit: `docs: record rewrite recovery and release evidence`.

## 8. Verification Commands

Run targeted tests during each task, then this final gate:

```bash
cargo fmt --all -- --check
DATABASE_URL="$DATABASE_URL" cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo leptos build --release --bin-cargo-args=--locked --lib-cargo-args=--locked
python3 scripts/validate_mechanic_traceability.py
npm run test:browser
npm run test:browser:slice3
npm run test:browser:slice5
npm run test:browser:slice6
npm run test:browser:journey
```

Also verify manually/operationally:

- signed-out home/login/sign-up on desktop and mobile;
- authenticated side navigation and logout;
- two simultaneous accounts with different characters/campaigns;
- invitation, ready, wait-for-all, and start-with-AI flows;
- LLM disabled, fake, timed out, malformed, and budget-exhausted paths;
- exact custom-action retry charges once;
- browser/process restart resumes the same lobby/turn/revision;
- archived/deleted campaign and account/session cleanup behavior;
- no private/account data in public SSR, caches, logs, WASM, or another account's responses.

## 9. Acceptance Criteria

The rewrite is complete only when all statements are true.

### Pages and navigation

- `/` contains only introductory content and public links.
- Signed-out users see a top login link and can reach sign-up.
- Public/auth pages never render the side navigation.
- Authenticated application pages render an accessible side navigation with applicable links.
- Protected route attempts return to the requested safe route after successful login.

### Authentication

- Passwords are stored only as salted Argon2id PHC hashes.
- Session tokens are random, cookie-only, and stored only as digests.
- Hosted cookies are Secure/HttpOnly/SameSite and mutations require CSRF.
- Login does not reveal whether an account exists.
- Logout, expiry, revocation, and password-change rotation are tested.

### Characters

- A player can create multiple rules-valid account-owned characters whose library records have no level, XP, HP, or campaign progression.
- Character list/detail/draft/mutation queries are account-scoped.
- Guessed foreign character IDs reveal and mutate nothing.
- Every character card/detail provides an accessible campaign display listing the authorized campaigns in which that character has a runtime instance.
- Clicking a campaign link shows that instance's current level and applicable stats; it never substitutes another campaign's stats.
- A single library character may have different levels and stats in different campaigns.
- Campaign runtime state is isolated from the reusable, level-less character-library source and never written back to it.

### Campaigns and lobby

- A player sees only owned/member campaigns and valid invitations.
- The campaign creator can create a lobby and invite/add players through the selected mechanism.
- **Wait for all** does not start while an active member is unready.
- **Start with AI substitutes** identifies missing members, requires confirmation, and records control mode durably.
- A returning player takes control only at an audited safe turn boundary.

### Play

- The dedicated play page alternates GM generation and player action phases.
- Only the active authorized player can submit that character's action.
- Structured legal actions remain free.
- Accepted custom prompts spend exactly the configured points in the same authoritative transaction; invalid/failed/replayed prompts do not double-charge.
- Missing-player AI can select only legal actions and cannot mutate state directly or spend points.
- Provider failure has a deterministic playable fallback.
- Reload/restart resumes the exact saved revision, turn actor, points, dice, mechanics, and history.

### Security and compatibility

- Two-account isolation passes for every server/API/asset route.
- Existing local campaign, hero, audit, and export fixtures remain readable.
- Hosted mode remains disabled until the documented gate passes.
- Accessibility, hydration, CSP, artifact secret scans, migrations, backup/restore, and full browser journeys pass.

## 10. Main Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| UI split ships before authorization | Implement principal/session/object authorization before exposing protected data routes. |
| Fixed local IDs remain in a hidden server function | Search/replace only after adding cross-account tests; retain explicit local wrappers, not implicit constants. |
| Character library and campaign progression conflict | Use account-owned source characters plus campaign-bound runtime instances. |
| Start race creates duplicate play sessions | PostgreSQL unique active-session constraint, row locks, revision checks, idempotency receipts. |
| AI substitute exceeds player authority | Purpose-specific typed proposal, legal-action allowlist, deterministic fallback, no point access. |
| Custom points charge without a committed action | Spend in the same transaction as mechanics/audit/receipt; replay by receipt. |
| Session cookie works locally but is unsafe remotely | Separate local/hosted cookie profiles; hosted requires canonical HTTPS origin and Secure cookies. |
| Password hashing causes denial of service | Benchmark Argon2id, bound concurrent hash work, throttle before expensive verification without creating timing enumeration. |
| Old saves become unreadable | Additive migrations, local compatibility account, schema fixtures/upcasters, backup/restore drill. |
| Monolithic component merely moves intact | Extract route-level views and focused components while keeping deterministic domain/application boundaries. |

## 11. Deferred Follow-up

Not part of this plan unless promoted explicitly:

- verified-email delivery and password reset;
- MFA/passkeys/OAuth;
- public campaign discovery/share links;
- spectators and public recaps;
- full human-authored GM mode;
- chat, WebSocket presence, and notifications;
- characters progressing across several campaigns from one shared mutable state;
- distributed session/rate-limit infrastructure for multiple web instances;
- administrative account recovery/moderation UI.
