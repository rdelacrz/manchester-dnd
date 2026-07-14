# Manchester Arcana implementation checklist

Status snapshot: 2026-07-14. This is the implementation backlog for the current repository, ordered by the dependency sequence in the [delivery roadmap](planning/08-delivery-roadmap.md). It covers the full MVP and keeps later ambitions visibly separate.

The planning documents remain authoritative for design intent. When an item here conflicts with a resolved architecture or policy decision, update both this checklist and the relevant planning document rather than silently changing behavior.

## How to use this checklist

- Every unchecked item is remaining work. Existing foundations are summarized below and are not repeated as tasks to rebuild.
- Complete work in slice order unless a later task is an isolated prerequisite or risk proof.
- A checkbox is complete only when the path is wired through the real Leptos UI, server/application boundary, deterministic engine, persistence where applicable, and proportionate tests. A type or adapter that no playable path uses is not complete.
- Link implementation, automated tests, manual evidence, migrations, and decision records from the checkbox or its pull request as work lands.
- Never expose a character option, action, spell, creature, content pack, or AI capability until every mechanic it can reach is implemented and capability-tested.
- Preserve the architectural invariants in the [planning index](planning/README.md): Rust owns mechanics, campaigns pin semantic versions, generated content has deterministic fallbacks, private inspiration is revocable, and every distributable asset has provenance.

## Delivery order

| Stage | Player-visible outcome | Must be resolved first |
| --- | --- | --- |
| Slice 0 | Reliable full-stack application shell | Q02, Q18 |
| Slice 1B | One complete deterministic encounter | Q04, Q06, Q19 |
| Slice 2 | Themed character creation and level 1 to 2 | Q03 and final Q04 scope |
| Slice 3 | Constrained AI GM text | Q07, text portion of Q08, Q10 |
| Slice 4 | Owned, resumable, exportable campaigns | Q13, Q14, identity mode |
| Slice 5 | Consented real-life inspiration | Q11, Q12, provider privacy approval |
| Slice 6 | Non-blocking scene images | image portion of Q08, Q09, Q10 |
| Slice 7 | Private-group MVP release | Q15, Q16, Q20 and all release gates |

## Existing baseline

The following is already present and should be extended rather than recreated:

- Leptos 0.8/Axum SSR workspace with `app`, `frontend`, `server`, `game-core`, and `game-server` boundaries.
- `dotenvy`-loaded, secret-redacting `TEXT_LLM_*` and `IMAGE_LLM_*` profiles; disabled and OpenAI-compatible provider adapters; `thiserror` error families.
- Explicit loopback-only local mode, Host/Origin checks, anti-framing headers, hosted-mode fail-closed behavior, and liveness/readiness endpoints.
- SQLite campaign, character, turn-audit, generated-asset, and command-receipt storage with revision checks and transactional writes.
- Slice 1A's fixed level-1 hero and persisted `inspect-viaduct-runes` Wisdom check, including server-owned mechanics/dice/time, visible outcome, atomic audit, idempotent replay, conflict handling, and exact reload.
- Domain foundations for ability scores, d20 checks, attack rolls, action resources, XP thresholds/awards, strict AI GM proposals, provider-independent generation, and Markdown event loading/weighted eligibility.

These foundations do **not** yet amount to a complete encounter, character creator, live AI turn, durable generation worker, consent system, or production-ready hosted application.

## 0. Resolve product and policy blockers

Reference: [decision register](planning/11-decision-register.md). For every resolution, record the date, owner, alternatives, rationale, affected durable versions, migration impact, tests, and rollout implications.

### Required now or already overdue

- [ ] **Q02 — supported clients:** choose exact browsers/devices, WCAG target, initial language support, and UTC/local-time behavior; turn the answer into a tested support matrix.
- [ ] **Q04 — MVP rules options:** freeze the exact ancestry/race, class paths, backgrounds, equipment, spells, creatures, and level 1–2 features that the engine and content packs will support.
- [ ] **Q06 — lethality and defeat:** define unconsciousness, death saves, stabilization, defeat, recovery, and whether an explicitly labeled non-terminal safety option ships.
- [ ] **Q18 — analytics:** confirm that MVP telemetry is operational-only, with no behavioral tracking or campaign/private-content capture, or document a different consented policy.
- [ ] **Q19 — RNG disclosure:** decide what players see, what remains server-side, and how PRNG algorithm/seed-reference/cursor data is retained for deterministic audits.

### Required before the named slice

- [ ] **Q03 — ability-score method (before Slice 2):** select the licensed fixed method for MVP or specify a bounded, fully audited random alternative.
- [ ] **Q07 — AI-proposed checks (before Slice 3):** finalize difficulty-band-to-DC mapping and which high-stakes checks require confirmation before rolling.
- [ ] **Q08 — providers and private-input terms (Slices 3 and 6):** select the first text/image adapters and document provider retention, training, region, moderation, and private-input eligibility.
- [ ] **Q09 — image invocation and budget (before Slice 6):** choose manual/on-demand versus automatic generation and define per-campaign cost/rate caps.
- [ ] **Q10 — presentation regeneration (Slice 3):** define retry limits, version visibility, private retention, export behavior, and the invariant that mechanics never reroll.
- [ ] **Q11 — audience and sensitive topics (before external testing):** set age restriction, prohibited categories, and escalation policy.
- [ ] **Q12 — consent administration (before Slice 5):** define participant identity mapping, out-of-band verification, and who may grant/revoke consent.
- [ ] **Q13 — retention (before Slice 4 schema freeze):** set retention for campaigns, attempts, diagnostics, audits, sources, artifacts, exports, and backups.
- [ ] **Q14 — visibility/sharing (before Slice 4):** confirm private authenticated exports only for MVP and explicitly defer any redacted public-share projection.
- [ ] **Q15 — project/content licenses (before public distribution):** choose code and original content/pack licenses and contribution terms.
- [ ] **Q16 — product branding (before public release):** complete name/domain/trademark review and preserve an original visual identity.
- [ ] **Q17 — generated-content promotion (before creator tooling):** define the human rights/safety/provenance review required to publish generated NPCs or locations into immutable packs.
- [ ] **Q20 — old-save support (before first public release):** define a compatibility and end-of-support policy for documents, audits, rulesets, and content packs.

## 1. Slice 0 — finish the full-stack walking skeleton

References: [roadmap](planning/08-delivery-roadmap.md), [architecture](planning/02-architecture.md), and [quality/security plan](planning/09-quality-observability-security.md).

### Browser and Leptos boundary

- [ ] Finish and document the supported-browser SSR/hydration test matrix from Q02.
- [ ] Prove the production SSR response hydrates with zero console or hydration warnings in each supported browser and viewport.
- [ ] Add deterministic hydration tests for server/client values, especially IDs, timestamps, randomized presentation, and configuration-derived branches.
- [ ] Finish the semantic application shell with navigation, campaign/scene placeholders, accessible loading/error states, and a real error boundary.
- [ ] Verify at least one core form works without WASM and gains enhancement after hydration without changing validation or authority.
- [ ] Add keyboard, focus, screen-reader, zoom, contrast, reduced-motion, and touch-target baselines to CI/manual evidence.

### Public boundary and configuration

- [ ] Apply explicit request-body, field-length, collection-size, and content-type limits to every server function and HTTP endpoint.
- [ ] Keep same-origin/local-mode enforcement covered end to end and document that another local process is outside the trust boundary.
- [ ] Add hosted-mode CSRF, secure-session, and campaign-authorization controls when hosted mode is introduced in Slice 4; keep hosted startup failing closed until then.
- [ ] Verify missing/invalid required environment values fail startup with field-specific diagnostics while values and credentials remain redacted.
- [ ] Add automated scans proving configured secrets are absent from WASM/client artifacts, SSR HTML, headers, errors, logs, and checked-in files.
- [ ] Persist non-secret configuration fingerprints with any retained generated output once generation is live.

### CI, deployment, and operations

- [ ] Add CI jobs for formatting, Clippy with warnings denied, unit/integration tests, Leptos SSR/WASM builds, migration validation, and documentation/link checks.
- [ ] Add a provider-disabled deployment/build smoke test that loads the application and completes its non-AI path.
- [ ] Extend structured tracing so one correlation ID follows HTTP request → server function → application command → database transaction → turn audit.
- [ ] Add redaction tests and canary values for errors/traces; never log prompts, secrets, source Markdown, or generated binary bodies by default.
- [ ] Prove readiness reports database failure separately from liveness, while documenting that readiness does not yet assert disk/backup/provider health.
- [ ] Record build/runtime instructions, supported ports, environment variables, local data location, and safe recovery steps in operator documentation.

### Slice 0 acceptance evidence

- [ ] Attach browser/hydration and accessibility evidence for the supported matrix.
- [ ] Attach client/response/log secret-scan evidence using injected canary credentials.
- [ ] Demonstrate invalid configuration, invalid server input, database-unready, and provider-disabled behavior with stable safe error codes.
- [ ] Demonstrate the local boundary rejects a forged Host/Origin and that hosted mode cannot accidentally start unauthenticated.

## 2. Slice 1B — complete one deterministic playable encounter

References: [roadmap Slice 1](planning/08-delivery-roadmap.md), [rules/gameplay](planning/03-rules-and-gameplay.md), [persistence](planning/05-persistence.md), and [licensing/provenance](planning/10-licensing-and-provenance.md). Slice 1A's exploration check is complete; the work below turns it into an encounter.

### Rules profile and traceability

- [ ] Create the machine-readable mechanic traceability skeleton: mechanic ID → `srd-5.1-cc` source location → implementation → tests → consuming content.
- [ ] Import or author only the minimal approved SRD-derived definitions needed for the encounter, with source keys, modification notes, license classification, and provenance.
- [ ] Define stable namespaced IDs and engine capability IDs for the prebuilt hero, creature, actions, attacks, damage, health transitions, and encounter objectives.
- [ ] Add a capability audit that fails when the encounter or prebuilt hero references an unsupported mechanic.

### Deterministic dice and roll records

- [ ] Implement a bounded dice-expression grammar for the shipped mechanics, including count/side/constant limits and overflow rejection.
- [ ] Pin a deterministic PRNG algorithm and persist its algorithm ID, protected seed material/reference, and cursor transitions according to Q19.
- [ ] Introduce a canonical `RollRecord` containing expression, individual/kept dice, modifier components, total, purpose, actors/targets, roll mode, ruleset version, and cursor range.
- [ ] Complete advantage/disadvantage cancellation and attack-specific natural 1/natural 20/critical handling without applying critical rules to generic checks.
- [ ] Ensure rendering, reload, retry, history, and correction paths consume stored rolls and never advance the RNG.

### Encounter engine

- [ ] Define canonical encounter state: participants, creature state, initiative, round, current actor, positions/ranges, objectives, and completion/defeat status.
- [ ] Finish the original prebuilt hero and one original/licensed creature with only supported capabilities.
- [ ] Implement initiative ordering, ties, round transitions, current-actor enforcement, and end-turn processing.
- [ ] Wire the existing action-economy primitives into movement, action, supported bonus action/reaction/object interaction, and per-turn reset rules.
- [ ] Implement the encounter's legal action set, at minimum movement, attack, any required contextual action, and end turn; reject unavailable actions and invalid targets server-side.
- [ ] Wire attack resolution through range/target validation, AC, hit/miss, critical result, damage roll, damage type, and deterministic explanation.
- [ ] Implement current/max/temporary HP mutation and the Q06 unconscious/defeat/recovery subset used by this encounter.
- [ ] Add authored success/failure consequences for the exploration check so it changes encounter state rather than presentation alone.
- [ ] Implement encounter objectives, victory/defeat completion, reward eligibility, and a stable transition back to exploration.
- [ ] Provide deterministic authored GM scene, action-result, victory, defeat, unsupported-action, and provider-unavailable text.

### Commands, persistence, and audit

- [ ] Add strict shared intent-only commands for initiative/combat actions; reject unknown fields and all client-supplied dice, DC, AC, modifiers, damage, HP, XP, actor, and timestamp values.
- [ ] Extend the application service to derive legal mechanics from pinned content, resolve in Rust, and commit state/revision/audit/idempotency receipt atomically.
- [ ] Expand immutable turn audits to include command identity, actor, state delta/facts, full roll records, rules/content/schema pins, and explanations.
- [ ] Make duplicate command retries return the original committed encounter result and make idempotency-key reuse with different intent fail safely.
- [ ] Preserve optimistic revision conflicts without consuming dice or partially mutating encounter/character state.
- [ ] Add correction events/revisions instead of permitting edits to historical rolls or audits.
- [ ] Create canonical fixtures proving browser/server restart loads the exact encounter revision, turn, HP, RNG cursor, rolls, and outcome.

### Playable Leptos UI

- [ ] Render the committed scene, prebuilt hero, creature/target state, initiative order, round/current actor, HP, movement/action resources, and encounter objective.
- [ ] Render only legal structured actions while keeping unsupported free-form input clearly unavailable until Slice 3.
- [ ] Add an accessible roll presentation and “why this result?” view showing dice, modifiers, AC/DC, damage, source key, and outcome.
- [ ] Show saving, saved, retrying, conflict, stale-view, completed, and deterministic degraded states without hiding authoritative results.
- [ ] Recover from a stale revision by offering reload/reconcile; never silently resubmit a mechanically different command.

### Slice 1 acceptance evidence

- [ ] Add golden vectors for initiative, action economy, attack, critical damage, HP transition, completion, and every encounter branch.
- [ ] Add property tests for dice bounds, roll totals, resource non-overspend, HP invariants, legal actor/target enforcement, and deterministic replay.
- [ ] Add integration/concurrency tests for duplicate submission, stale revision, transaction rollback, and restart/reload equivalence.
- [ ] Add browser E2E coverage that plays the encounter to success and defeat, reloads mid-combat, and verifies no client can forge rolls or act out of turn.
- [ ] Demonstrate fixed initial state + commands + dice source yields byte-equivalent canonical state/audit on repeated runs.

## 3. Slice 2 — themed hero creation and advancement

References: [roadmap Slice 2](planning/08-delivery-roadmap.md), [characters/content packs](planning/07-characters-and-content-packs.md), [rules matrix](planning/03-rules-and-gameplay.md), and [product MVP](planning/01-product-vision.md).

### Content-pack platform

- [ ] Define immutable `content-pack/v1` manifests with namespaced ID, version, digest, categories, compatible rulesets, engine capabilities, dependencies, license, provenance manifest, and bounded content roots.
- [ ] Define bounded schemas for rules definitions, themes, adventures, creatures, items, spells, prompt fragments, and assets; forbid executable code, arbitrary HTML/templates, and network fetches.
- [ ] Canonicalize all pack paths beneath an allowlisted root and enforce file/count/size/decompression/digest limits.
- [ ] Implement staged validation for schema, digest, dependencies, cycles, references, ruleset/capabilities, license/provenance, markup/instruction safety, mechanical fixtures, and render smoke tests.
- [ ] Quarantine invalid packs; activate only validated exact versions/digests; block removal while an active campaign depends on a version unless a readable archive remains.
- [ ] Pin exact rules/content/theme/prompt/schema versions to campaign creation and retain aliases/migrations for renamed choices.
- [ ] Add capability reports proving every reachable option, creature, item, action, and spell has engine support.
- [ ] Ship at least two original presentation-only theme packs with the same mechanical coverage and no theme-specific branches in `game-core`.
- [ ] Supply each theme's design tokens, original names/concepts, accessible descriptions/non-color cues, placeholder art direction, valid presets, and bounded untrusted prompt fragments.

### Licensed MVP content

- [ ] Finalize Q04's deliberately small level 1–2 rules/content set and hide every unsupported SRD option.
- [ ] Populate the `srd-5.1-cc` compendium subset from approved CC BY 4.0 sources, never copied from the reference-only 2018 Basic Rules PDF.
- [ ] Give every rule-bearing entry a mechanic ID, ruleset/schema version, typed effects, capability requirements, source key, license class, and provenance digest.
- [ ] Implement closed typed effects such as damage, healing, conditions, movement, resource spending, and clocks; keep reviewed special cases in named Rust policies.
- [ ] Complete the mechanic traceability table and make missing source/implementation/test/content links fail CI.

### Resumable character creator

- [ ] Persist non-authoritative, resumable creation drafts with ownership, expiry/retention, schema version, and safe conflict handling.
- [ ] Implement the server-validated steps: campaign/theme, concept, ancestry/race and class, ability scores, background/proficiencies, equipment/resources, identity/presentation, review, and commit.
- [ ] Implement Q03's ability-score method in Rust and display an audit for any randomized method.
- [ ] Filter choices by pinned ruleset, pack capabilities, prerequisites, duplicates, and mutually exclusive selections.
- [ ] Compute all derived values in Rust: modifiers, proficiency, AC, HP, saves, attacks, resources, equipment state, and supported spell summary.
- [ ] Provide a complete authored no-AI path; constrain optional AI suggestions to known valid mechanic IDs and discard invented/invalid IDs.
- [ ] Render provenance/source labels, unsupported limitations, legal-action preview, and level-up preview before commit.
- [ ] Commit `CharacterCreated`, explicit base choices, pins, derived-state version, and initial resources atomically; never promote a partial draft to authority.
- [ ] Reject forged or stale combinations server-side even if the UI would not normally offer them.

### Complete the MVP rules matrix for offered content

- [ ] Add saving throws, required skills, passive values, proficiency handling, and situational modifiers.
- [ ] Complete checks, saves, attacks, DC/AC, advantage/disadvantage, and auditable roll integration for every offered option.
- [ ] Complete exploration and combat turns, movement, action, supported bonus actions/reactions, object interaction, and duration processing.
- [ ] Implement contextual availability for Attack, Cast a supported spell, Dash, Disengage, Dodge, Help, Hide, Ready, Search, and Use an Object where shipped content can use them.
- [ ] Complete melee/ranged range rules, the required cover subset, critical damage, damage types, and required resistance/vulnerability/immunity interactions.
- [ ] Complete max/current/temporary HP, damage, healing, unconsciousness, death saves, stabilization, and rest recovery according to Q06.
- [ ] Implement prone, restrained, grappled, incapacitated, unconscious, poisoned, and every additional condition reachable from shipped content.
- [ ] Implement hit dice/rests and every class resource required by supported level 1–2 paths.
- [ ] Implement currency, carried/equipped items, supported weapons/armor/consumables, and the documented capacity policy.
- [ ] Implement the exact small SRD spell list selected in Q04 with explicit typed effects and tested targeting/resource rules.
- [ ] Implement exploration/social objectives, clocks, NPC attitude, constrained difficulty proposals, and trusted tier-to-DC mapping.
- [ ] Add deterministic actions/stat blocks for the complete small creature/encounter set.
- [ ] Return `unsupported_mechanic` with legal authored alternatives whenever an intent falls outside this matrix; never delegate the gap to the AI.

### Level 1 to 2 advancement

- [ ] Map trusted reward tiers through campaign policy to validated XP awards; never accept model/client XP amounts.
- [ ] Detect level-up eligibility from pinned XP thresholds and freeze incompatible adventure commands without corrupting an active encounter.
- [ ] Generate valid level-up choices for every supported class/content combination from the campaign's pinned versions.
- [ ] Validate prerequisites and commit level, choices, features, HP, resource maxima/current policy, and audit in one transaction.
- [ ] Store explicit choices so derived-state reconstruction never guesses and reload yields the same level-2 sheet.
- [ ] Define and test pack-version migration/alias behavior without silently changing existing characters.

### Slice 2 UI and acceptance evidence

- [ ] Make the full creation/advancement flow keyboard- and screen-reader-usable, responsive, recoverable after refresh, and clear about validation errors.
- [ ] Add combinatorial tests proving every offered creation/level-up combination is valid and every forged invalid combination is rejected.
- [ ] Test disabling either theme pack without changing engine mechanics or breaking existing pinned campaigns.
- [ ] Run pack validation, capability, provenance, deterministic encounter, hydration, and accessibility checks in CI.
- [ ] Add E2E coverage from new campaign → themed hero → exploration/combat → XP award → level 2 → reload.

## 4. Slice 3 — constrained AI GM text

References: [roadmap Slice 3](planning/08-delivery-roadmap.md), [AI generation](planning/04-ai-generation.md), [architecture](planning/02-architecture.md), and [rules authority split](planning/03-rules-and-gameplay.md).

### Typed AI boundary

- [ ] Split the existing general proposal seam into purpose-specific, versioned `ActionProposal`, `CheckProposal`, `SceneProposal`, narration, and clarification schemas.
- [ ] Keep strict unknown-field decoding, bounded strings/collections, known IDs, proposal IDs, session IDs, based-on revision/event sequence, and non-secret config fingerprint on every retained request/result.
- [ ] Harden application acceptance with campaign authorization, actor/target legality, locked expected revision, capability checks, hidden-information policy, safety policy, and trusted difficulty/reward mapping.
- [ ] Convert accepted proposals into ordinary engine commands; the model must never directly mutate HP, inventory, XP, DC, AC, rolls, turn order, or revision.

### Prompt assembly and free-form intent

- [ ] Build deterministic, versioned prompt assembly from system policy, rules profile, legal action IDs, minimized player-visible state, safety settings, selected theme fragments, and the current player intent.
- [ ] Delimit all player/content/event text as untrusted data and exclude secrets, hidden GM state, unrelated history, consent records, raw source Markdown, and credentials.
- [ ] Persist prompt template/schema/policy/model configuration fingerprints without storing sensitive prompt bodies by default.
- [ ] Parse free-form intent into typed proposals, reject ambiguity/unsupported mechanics, and ask focused clarification or present authored legal alternatives.
- [ ] Bound parse/repair attempts; never recursively trust model-authored instructions or tool/capability requests.

### Mechanics-first narration

- [ ] Complete and commit authoritative mechanics before requesting narration; narration failure must never strand or roll back the turn.
- [ ] Build a fact-only narration context from committed events/rolls and validate that generated prose preserves actor, targets, outcomes, numbers, conditions, inventory, and visibility.
- [ ] Escape/sanitize generated text for rendering and reject unsafe, contradictory, or unsupported claims.
- [ ] Provide deterministic templates for checks, attacks, damage, status changes, unsupported intent, clarification, victory/defeat, and provider outage.
- [ ] Apply closed GM planning allowlists and per-turn budgets to NPC actions, event proposals, and reward tiers.

### Execution, resilience, and cost controls

- [ ] Wire the existing environment-selected text adapter into the real turn flow with a deterministic fake used in tests and provider-disabled fallback in production.
- [ ] Add generation job/attempt metadata and stable idempotency relationships to the originating campaign revision/turn; finish durable worker recovery in Slice 4.
- [ ] Add purpose-specific deadlines, cancellation, bounded retry/backoff, concurrency limits, response-size limits, and a circuit breaker.
- [ ] Distinguish timeout, unavailable, rate-limited, malformed, unsafe, and contradiction failures in redacted internal metrics and stable client states.
- [ ] Enforce per-turn/per-campaign token, request, latency, and cost budgets; record non-secret usage/cost estimates.
- [ ] Show a clear degraded-mode UI that completes play/save with deterministic text and allows a bounded presentation retry under Q10.

### Evaluation and acceptance evidence

- [ ] Build a versioned synthetic corpus covering legal intents, ambiguity, unsupported requests, hostile prompt injection, malformed output, privacy leakage, stale proposals, and narration fidelity.
- [ ] Require schema-valid rate, proposal acceptance rate, clarification quality, fallback rate, contradiction rate, privacy/safety failures, latency, and cost to pass a promotion threshold per model/config fingerprint.
- [ ] Test that changing supported provider/model environment values plus restart requires no code change and never embeds credentials in fingerprints/artifacts.
- [ ] Run timeout, outage, malformed-output, and hostile-input E2E tests proving every turn remains mechanically correct and saveable.
- [ ] Prove model output cannot bypass the application service or become a trusted audit before engine validation/commit.

## 5. Slice 4 — durable campaign ownership

References: [roadmap Slice 4](planning/08-delivery-roadmap.md), [persistence/versioning](planning/05-persistence.md), [architecture evolution](planning/02-architecture.md), and [security controls](planning/09-quality-observability-security.md).

### Identity and authorization

- [ ] Select and document the MVP identity mode, then implement account identity, secure browser sessions, logout/revocation, and one-owner/one-hero campaign membership.
- [ ] Replace hosted-mode fail-closed stubs only after every campaign, character, turn, job, export, and artifact operation enforces object-level authorization.
- [ ] Use TLS in deployment; `Secure`, `HttpOnly`, and appropriate `SameSite` cookies; session rotation/expiry; login throttling; CSRF protection; and non-enumerating errors.
- [ ] Add authorization matrix tests proving guessed IDs cannot read or mutate another user's campaign, character, audit, job, export, or artifact.
- [ ] Retain explicit loopback local mode as a separate deployment profile; do not infer local trust from a hostname in hosted mode.

### Durable data model and mutation boundary

- [ ] Add campaign ownership/membership, play-session, rules/content/prompt/schema pins, safety/progression policy, and retention classification fields/tables.
- [ ] Add `generation_jobs` and `generation_attempts` with state, purpose, lease owner/expiry, retry time, attempt count, input/config digests, provider/model, usage, redacted error, and artifact relationship.
- [ ] Route every state-changing path—creation, play, XP/level-up, archive/delete, consent later, jobs, and artifact selection—through the application boundary with expected revision and idempotency receipt where appropriate.
- [ ] Store complete immutable turn audits with actors, intent, rules facts, rolls, deltas, pins, and generation references; add explicit correction audits instead of mutation.
- [ ] Define canonical JSON serialization and independently version campaign, character, turn audit, generated artifact, ruleset, content pack, prompt/schema, consent, and export formats.
- [ ] Add fixtures and migration dry-run tooling for every released durable version, with compatibility behavior driven by Q20.

### Save, resume, history, and export

- [ ] Show autosave pending/saved/conflict/failure state for every committed turn and recover safely after browser or server restart.
- [ ] Add campaign list/create/resume/archive UI and explicit play-session boundaries.
- [ ] Paginate ordered turn history and render roll/rule explanations exclusively from stored audits.
- [ ] Generate a player-readable private export with sheet, campaign state/summary, turns, dice, selected artifacts, pins, provenance, and attribution.
- [ ] Generate a canonical machine-readable restorable export of MVP documents/audits while excluding credentials, raw private sources, other participants' consent records, and unselected sensitive attempts.
- [ ] Implement explicit archive, restore-from-archive, delete, and derived-artifact cleanup behavior according to Q13/Q14.
- [ ] Treat a future public/shareable recap as a separately authorized, redacted post-MVP projection.

### SQLite reliability and recovery

- [ ] Configure and document foreign keys, WAL/checkpoint behavior, `synchronous` level, busy timeout, transaction mode, and bounded retry classification.
- [ ] Ensure transient busy/locked retry preserves the original idempotency key and never rerolls or duplicates an audit.
- [ ] Keep the database and generated files on least-privilege persistent paths outside public/static roots.
- [ ] Implement coordinated online backup/checkpoint procedures, encrypted backup storage, retention/expiry, integrity checks, and an operator restore command/runbook.
- [ ] Monitor migration version, DB/WAL size, write latency, busy/locked errors, disk capacity, integrity results, backup age, and last restore-test result.
- [ ] Test abrupt termination around commits, corrupt/unknown JSON, constraint failures, disk full, migration failure, expired job lease reclamation, and backup restoration.
- [ ] Prove restored sampled campaigns have matching canonical state hashes, complete histories, valid pins, and readable protected assets.

### Slice 4 acceptance evidence

- [ ] Complete cross-user authorization/ID-enumeration tests at every route and server function.
- [ ] Load every supported old fixture after migrations with unchanged mechanical meaning.
- [ ] Export and restore a representative advanced campaign, verifying revisions, rolls, provenance, and exclusions.
- [ ] Run and document archive/delete/backup-expiry behavior against the Q13 retention policy.
- [ ] Demonstrate durable text job recovery/idempotency across process restart without blocking deterministic play.

## 6. Slice 5 — consented real-life inspiration

References: [roadmap Slice 5](planning/08-delivery-roadmap.md), [consent/privacy/safety](planning/06-consent-privacy-safety.md), [AI boundary](planning/04-ai-generation.md), and [persistence](planning/05-persistence.md). The strict Markdown v1 loader and in-memory eligibility/weighted selector exist; everything below is required before real sources are enabled.

### Feature gate, ingestion, and source registry

- [ ] Add a deployment and per-campaign feature flag that defaults off and cannot be enabled without the consent/safety prerequisites.
- [ ] Read only configured files under a canonical allowlisted root; reject symlinks, traversal, invalid UTF-8, active resources, and excessive file/count/size inputs.
- [ ] Scan metadata/body for likely names, contact details, addresses, employers, account handles, direct quotations, and prohibited sensitive categories.
- [ ] Quarantine uncertain/failed sources for explicit human review; never infer approval from Git/filesystem access.
- [ ] Convert approved bodies into minimized neutral facts, while retaining raw text only in the protected source location.
- [ ] Register opaque source ID, digest/version, schema, owner, tags, participants, review/signature, eligible media/audience, expiry, and provenance; revalidate every changed digest.

### Consent and campaign safety

- [ ] Persist independent participant consent records scoped by source, campaign, audience, media, transformations, fictional distance, sensitivity, expiry, reviewer, and post-revocation artifact policy.
- [ ] Implement the Q12 verified pseudonymous participant mapping without exposing another person's consent details.
- [ ] Add campaign safety setup for tone/age, lines, veils, topics/phobias, inspiration on/off, participant exclusions, and high fictional distance by default.
- [ ] Enforce conservative defaults: no minors, likeness, current crisis, or health/trauma/sexual/criminal/financial/relationship secrets.
- [ ] Make missing, incomplete, revoked, expired, wrong-audience, or wrong-media consent deterministically ineligible regardless of event weight.

### Deterministic selection and fictionalization

- [ ] Open authored trigger windows only at safe narrative boundaries and block incompatible combat/safety states.
- [ ] Filter by campaign opt-in, all participants, media/audience, expiry, active source version, safety settings, theme, cooldown, recent use, and vetoes before weighting.
- [ ] Use the server-owned deterministic RNG and persist draw, eligible-set digest, opaque selected source/version, cooldown update, and no-selection reason for replay/audit.
- [ ] Pass only minimized facts plus a bounded transformation policy to the model; never send raw Markdown, consent databases, filesystem paths, contact data, or unrelated history.
- [ ] Enforce high fiction distance: replace identities/roles, remove exact sequence/wording/dates/locations/appearance, and forbid sensitive inference or embellishment.
- [ ] Run output identifier, quotation/similarity, safety, and consent checks; discard failures and use an unrelated deterministic fallback without repeatedly probing the private source.

### Player controls and data rights

- [ ] Add always-visible pause, veil/hide, veto, source/category/all-inspiration disable, and privacy-report controls using only opaque IDs.
- [ ] Honor veto immediately without asking for justification, hide presentation, cancel pending derived work, and continue with an unrelated fallback.
- [ ] Apply revocation to eligibility immediately and hide/delete/redact derived text/images according to policy without rewriting mechanical history.
- [ ] Provide authorized source inventory, access, correction, consent review, export, revocation, and deletion workflows without revealing another person's body text.
- [ ] Implement audience visibility review and keep public share links unavailable in MVP.

### Security, privacy operations, and acceptance evidence

- [ ] Encrypt protected source storage/backups and separate decryption access from ordinary game/image workers where practical.
- [ ] Audit install/update, review, consent change, selection, artifact creation, restricted diagnostic access, revocation, and deletion with opaque IDs only.
- [ ] Add a global kill switch and incident runbook for disabling generation, invalidating access, quarantining artifacts, rotating credentials, notifying users, and preserving minimal evidence.
- [ ] Prove raw source text and identifiers never enter client bundles, normal network responses, logs, metrics, analytics, exports, support artifacts, or evaluation corpora.
- [ ] Add hostile Markdown/prompt-injection tests proving source content cannot alter system policy or invoke capabilities.
- [ ] Add deterministic/statistical tests proving every ineligible source has zero selection probability and cooldown/selection replay is stable.
- [ ] Run a complete veto, revocation, pending-job cancellation, derived-artifact deletion/redaction, export, live-data deletion, and documented backup-expiry exercise.
- [ ] Keep the feature off until threat modeling, provider-policy review, user testing, and all release-gate evidence pass.

## 7. Slice 6 — asynchronous scene images

References: [roadmap Slice 6](planning/08-delivery-roadmap.md), [image flow](planning/04-ai-generation.md), [generation architecture](planning/02-architecture.md), and [consent safety](planning/06-consent-privacy-safety.md). The provider adapter exists; this slice makes generation durable, safe, private, and non-blocking.

### Brief and policy boundary

- [ ] Define a versioned, bounded `ImageBrief` from already committed visible facts, art direction, composition, exclusions, safety rating, fictionalization policy, and alt-text context.
- [ ] Exclude raw inspiration sources, real names/likenesses, hidden state, secrets, unrelated history, and provider instructions from image briefs.
- [ ] Enforce Q09 invocation mode and per-campaign/request budget before enqueueing.
- [ ] Apply pre-generation safety/rights/likeness checks and record only non-sensitive policy decisions/digests.

### Durable jobs and worker

- [ ] Enqueue a durable image job linked idempotently to campaign revision/turn/brief fingerprint and return a placeholder immediately.
- [ ] Implement transactional lease claim/renewal/expiry, bounded attempt count/backoff, cancellation, retry scheduling, and terminal states.
- [ ] Wire the existing environment-selected image adapter into a separately bounded worker with deterministic fake/disabled providers for tests.
- [ ] Make refresh, duplicate request, worker crash, server restart, timeout, provider rejection, and lost lease recover without losing or duplicating artifacts.
- [ ] Enforce provider deadlines, concurrency, response byte limits, redirect policy, egress allowlists/SSRF protection, rate limits, and circuit breaking.

### Artifact validation, storage, and delivery

- [ ] Decode into a quarantine area and verify actual format/signature, MIME, dimensions, pixel/decompression limits, and safety result before publication.
- [ ] Strip metadata and produce bounded web variants/thumbnails; never render provider URLs or unverified bytes directly.
- [ ] Store originals/variants under protected non-public paths with digests and object-level authorization.
- [ ] Persist provider/model/config/prompt-policy fingerprints, creation time, source turn, dimensions/MIME, hashes, moderation result, selected/superseded state, cost, and license/provenance data.
- [ ] Generate, validate, store, and render meaningful alt text for every displayed image.
- [ ] Quarantine unsafe/invalid outputs, prevent unauthorized fetches, and implement retention/deletion under Q10/Q13.

### UI, budgets, and acceptance evidence

- [ ] Add an accessible request control, placeholder, queued/running/retry/rejected/unavailable states, cancel action, and polling or streamed completion updates.
- [ ] Keep the turn fully playable/saveable while image work is queued, slow, rejected, rate-limited, or disabled.
- [ ] Show owner-visible usage/cost estimates and enforce campaign/account hard caps without leaking provider internals.
- [ ] Test restart/lease recovery, duplicate enqueue, cancellation races, unsafe results, spoofed MIME, oversized/decompression-bomb input, malicious redirect/URL, and authorization.
- [ ] Prove every displayed artifact has a verified variant, alt text, provenance record, authorized delivery path, and no private-source/likeness violation.

## 8. Slice 7 — MVP release gate

References: [roadmap Slice 7](planning/08-delivery-roadmap.md), [quality/observability/security](planning/09-quality-observability-security.md), [licensing/provenance](planning/10-licensing-and-provenance.md), and [product success measures](planning/01-product-vision.md).

### Rules and product completeness

- [ ] Verify every MVP requirement and every reachable mechanic/content entry has linked implementation, source, tests, UI evidence, and capability coverage.
- [ ] Run the complete internal journey: create campaign/hero, play exploration/social/combat, receive inspiration, request an image, reach level 2, inspect history, export, stop, restart, and resume without database repair.
- [ ] Add a guided first-run flow, supported-features/limitations page, safe setup, privacy explanation, and feedback/privacy reporting route.
- [ ] Measure successful creation, resume, turn completion, level-up, and deterministic degraded play against the success measures; do not collect campaign content to do so.

### Test portfolio

- [ ] Complete domain unit and property tests for ability/derived values, dice, saves, attacks, damage, health, action economy, conditions, equipment, spells, and advancement.
- [ ] Complete deterministic RNG golden vectors and canonical save/audit/export compatibility fixtures.
- [ ] Complete pack schema/capability/provenance, persistence/migration/repository, API/server-function, hydration/component, and browser E2E suites.
- [ ] Complete AI schema/fidelity/injection/privacy evaluation suites and provider fake/timeout/outage/chaos tests.
- [ ] Fuzz dice expressions, durable JSON/schema decoding, pack/event Markdown ingestion, model-output parsing, image metadata/decoding boundaries, and public input parsers.
- [ ] Run load/soak tests for turn commits, idempotent concurrency, history/export, job leases, worker concurrency, and provider limits.
- [ ] Make CI fail for missing rules traceability, unsupported reachable capabilities, stale golden fixtures, unsafe dependency/license findings, secret canaries, or broken planning/document links.

### Observability and operations

- [ ] Carry correlation IDs through HTTP → command → database → audit → generation job/attempt → provider → artifact without logging sensitive bodies.
- [ ] Emit bounded metrics for request/turn/job outcomes, latency, validation/fallback rates, DB busy/size/backup health, queue depth/lease age, provider usage/cost, and authorization/safety denials.
- [ ] Establish evidence-based SLOs and dashboards only after representative private-test measurements; alert on sustained user impact rather than individual expected failures.
- [ ] Add tested runbooks for database restore, migration rollback/read-only operation, provider outage/degraded mode, queue recovery, disk full, credential rotation, consent incident, artifact quarantine, and deletion requests.
- [ ] Rehearse backup restore and release rollback without making newly committed campaigns unreadable.

### Security and privacy release work

- [ ] Produce a threat model covering browser/server trust, local versus hosted mode, authentication/object authorization, CSRF/XSS, prompt injection, private sources, provider egress, jobs, artifacts, exports, and backups.
- [ ] Finish CSP, anti-framing, output sanitization, request/upload/response limits, rate limits, secure-cookie/TLS settings, and cache controls for private responses/artifacts.
- [ ] Verify WASM, source maps, hydration payloads, service-worker/browser caches if any, errors, and diagnostics contain no secrets, hidden state, or private source material.
- [ ] Review database/file permissions, backup encryption, key separation/rotation, outbound host allowlists, redirect behavior, provider credentials, and least-privilege worker access.
- [ ] Run penetration-focused authorization, ID enumeration, CSRF, XSS, SSRF, upload/parser, prompt-injection, and artifact access tests.
- [ ] Run incident, provider outage, backup restore, credential rotation, pack/source quarantine, consent revocation, and user deletion drills.
- [ ] Publish a security/privacy reporting contact and private-test data-handling/retention documentation.

### Licensing, attribution, and provenance

- [ ] Preserve the exact SRD 5.1 CC BY 4.0 attribution/preamble required by the source in notices/credits and an accessible in-app legal view.
- [ ] Identify SRD-derived files/content, source URLs/versions, modifications, and mechanic-level provenance without copying excluded Basic Rules/D&D-branded material.
- [ ] Maintain a machine-readable provenance manifest for every rules/content/prompt/image/font/icon/audio/file asset with class, author/source, license/terms, digest, transformations, and campaign/pack references.
- [ ] Review provider generated-content ownership, retention/training, likeness, moderation, takedown, and output-similarity terms before enabling each deployment profile.
- [ ] Complete Q15/Q16 license and branding review; avoid protected product identity, endorsement implications, real-person/business branding, and unlicensed trade dress.
- [ ] Inventory Rust/JS/build/container/font/icon dependencies, generate an SBOM, preserve notices/source offers, and resolve incompatible/unknown licenses.
- [ ] Define contribution and third-party pack intake requirements for ownership, license, consent, provenance, generated portions, and takedown.
- [ ] Add automated provenance/license/SBOM gates plus a human-readable release report; block release on unknown required provenance.

### Supply chain, accessibility, and release decision

- [ ] Pin/review Rust toolchain, dependencies, CI actions, build images, and lockfiles; run vulnerability/advisory and license scans.
- [ ] Build a minimal runtime artifact, scan it for secrets/vulnerabilities, and sign/record release provenance where supported.
- [ ] Complete manual stable-browser/mobile, keyboard, screen-reader, zoom, contrast, reduced-motion, responsive-layout, error-recovery, and slow-network passes against Q02.
- [ ] Verify every generated/persistent visual has an alternative and all dice/result information is understandable without color, motion, or image access.
- [ ] Close or explicitly block release for every critical security, privacy, data-loss, rules-correctness, licensing, accessibility, and consent issue.
- [ ] Link automated/manual evidence for every Slice 0–7 acceptance criterion and record the private-group release decision/rollback plan.

## 9. Post-MVP backlog — not an MVP release blocker

Reference: [post-MVP roadmap](planning/08-delivery-roadmap.md) and [later ambitions](planning/01-product-vision.md). These remain unchecked by design until the MVP is released and measured.

- [ ] Expand SRD 5.1 options/mechanics in complete source-traced, capability-tested bundles and progress from levels 1–2 toward levels 1–20.
- [ ] Add invited multiplayer: per-character ownership, concurrent commands, turn notifications, presence, shared consent, moderation, and seed commitment/reveal where competitive claims require it.
- [ ] Build creator tooling: authoring/validation CLI, signed immutable packs, quarantine/review workflow, distribution policy, and approved generated-content promotion under Q17.
- [ ] Add richer media such as portraits, maps, voice/accessibility narration, audio, and consistency tools, each with independent consent/rights/safety gates.
- [ ] Add campaign checkpoints, recaps, branching/replay, and—only if justified—a complete append-only event stream with dual-write/equivalence proofs.
- [ ] Add explicitly versioned homebrew rules packs and optional milestone advancement without silently changing `srd-5.1-cc` campaigns.
- [ ] Implement SRD 5.2.1 as a separate rules adapter, compendium, tests, campaign-creation option, and explicit conversion report/workflow.
- [ ] Add more provider adapters, local models, routing, and optional hot configuration only with equivalent safety/evaluation/fingerprint behavior.
- [ ] Design a separately authorized and redacted public-share projection; never expose the private canonical campaign/export directly.
- [ ] Migrate to PostgreSQL/object storage only after measured SQLite or multi-instance limits, repository contract tests, verified export/import, and rehearsed rollback justify it.

## Completion rule for every implementation checkbox

Before marking an implementation item complete, verify all applicable statements:

- [ ] The feature is reachable through the real user flow, not only through an unused type, adapter, fixture, or test helper.
- [ ] The server/application boundary authorizes and validates it; clients and models submit intent only.
- [ ] Deterministic mechanics and canonical state are reproducible from pinned inputs and stored rolls without regeneration.
- [ ] Durable writes are atomic, revisioned/idempotent as appropriate, migration-compatible, and recoverable after restart/failure.
- [ ] Safe errors, logs, metrics, exports, prompts, and artifacts exclude credentials, hidden state, raw private sources, and unrelated personal data.
- [ ] Unit/integration/property/E2E/manual evidence is proportionate to risk and linked from the task or change.
- [ ] Accessibility, degraded/offline-provider behavior, licensing/provenance, and documentation are complete for the exposed surface.
- [ ] Relevant planning documents, decision records, traceability data, schemas, fixtures, and this checklist are updated in the same change.
