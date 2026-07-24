# Delivery roadmap

## Delivery rule

Build vertical slices that can be demonstrated through the real Leptos UI, server boundary, domain engine, and persistence path. A partially implemented subsystem does not count as progress if no playable path uses it. Each slice leaves the main branch deployable and retains deterministic provider fakes.

The sequence below is dependency order, not a calendar estimate. Slices 0–7 together are MVP. The repository contains the Leptos workspace, deterministic core/server crates, MongoDB repository/schema catalog, optional Dragonfly cache/pub-sub, typed LLM profiles, rules types, authentication, and server-authoritative checks. “Deliver” includes proving that baseline, not rebuilding it under speculative crate names.

Progress as of 2026-07-24: the walking skeleton has authenticated local/hosted configuration, redacted errors, liveness/readiness probes, MongoDB transactional persistence, Dragonfly-safe degradation, and persisted gameplay. Remaining acceptance work is tracked in the checklist rather than inferred from this summary.

## Slice 0 — Full-stack walking skeleton

**Player value:** the real web application loads reliably and reports actionable configuration/service errors.

Deliver:

- Rust workspace and crate boundaries from [architecture](02-architecture.md);
- Leptos 0.8/Axum SSR, hydration, routing, semantic shell, error boundary, and one progressive-enhancement form;
- MongoDB replica-set connection, repository, managed validators/indexes, and optional Dragonfly cache/pub-sub;
- typed startup configuration using `dotenvy`, `.env.example`, and secret-redacting types;
- initial `thiserror` families and safe transport mapping;
- tracing/correlation IDs, readiness/liveness checks, CI formatting/lint/test/build, and a provider-free deployment.

Acceptance:

- SSR HTML loads and hydrates with zero console/hydration warnings in supported browsers;
- the WASM/client artifact and HTTP responses contain no configured secret;
- missing/invalid required configuration fails startup with field-specific diagnostics;
- readiness distinguishes database failure from process liveness;
- one server function rejects invalid input safely and enforces the chosen authenticated or explicit local-mode boundary end to end.

## Slice 1 — One deterministic playable encounter

**Player value:** choose an action, see a real roll and explanation, finish a small encounter, reload it.

**Implemented Slice 1A:** the local page lazily creates/resumes a fixed level-1 hero; `inspect-viaduct-runes` resolves as a server-owned Wisdom (proficient) DC 13 check; a strict command cannot carry mechanics; revision, `AbilityCheckResolved` audit, and idempotency receipt commit atomically; matching retries do not reroll; reload projects the stored result. Initiative, creature state, movement/action turn enforcement, attack/damage, HP mutation, encounter completion, pinned RNG stream/cursor, and authored consequence state remain pending below.

Deliver:

- pinned `srd-5.1-cc` profile and source traceability skeleton;
- prebuilt hero, one original creature, one exploration check, initiative, movement/action budget, attack, damage, HP, and encounter completion;
- bounded dice parser, injected/versioned dice source, roll records, command and turn-audit types;
- revisioned MongoDB campaign/character documents, append-only turn audits, short transactions plus optimistic revision checks, idempotent command, and load;
- authored deterministic GM text and visible “why this result?” details.

Acceptance:

- golden rules vectors and property tests cover every shipped mechanic;
- the client cannot submit a die value or attack out of turn;
- fixed dice source + commands produces the same canonical result/state/audit on repeated runs;
- browser reload resumes at the exact revision with the same dice and outcome;
- duplicate submission commits once; stale revision produces a recoverable conflict.

## Slice 2 — Themed hero journey and advancement

**Player value:** create a personal hero in an original theme, complete exploration/combat, and reach level 2.

Deliver:

- two original theme packs, immutable manifests, provenance, compatibility/capability validation;
- resumable character-creation draft and atomic commit;
- supported ability/class/background/equipment choices, derived sheet, legal-action panel;
- the complete MVP rules matrix required by those choices and encounter content;
- the implemented XP advancement policy plus a complete level 1→2 choice workflow for every offered option.

Acceptance:

- every combination the UI offers creates a rules-valid character; invalid forged combinations are rejected server-side;
- each pack can be disabled without theme-specific engine conditionals;
- an encounter/content capability audit finds no reachable unsupported mechanic;
- level-up applies choices/HP/resources atomically and reload derives the same level-2 sheet;
- creation and play are keyboard-usable and pass agreed automated accessibility checks.

## Slice 3 — AI GM text, safely constrained

**Player value:** type free-form intent and receive responsive, varied GM prose without sacrificing rules integrity.

Deliver:

- text-provider adapter selected by runtime environment configuration;
- purpose-specific structured schemas for intent, check/scene proposals, and narration;
- minimized prompt assembly, untrusted-input delimiting, strict validation, bounded repair;
- mechanics-first commit followed by narration; deterministic fallbacks and degraded-mode UI;
- model config fingerprint, job/attempt metadata, cost/latency/validation metrics, and synthetic evaluations.

Acceptance:

- a model can propose but cannot directly change HP, inventory, XP, DC, rolls, or campaign revision;
- malformed/hostile output never becomes authoritative state or a trusted turn audit and falls back or asks for clarification;
- provider timeout/outage still permits a complete turn and save;
- narration fidelity tests catch changed outcomes/numbers and critical prompt-injection/privacy suites pass;
- changing supported provider/model environment values plus restart requires no code change.

## Slice 4 — Durable campaign ownership

**Player value:** manage campaigns, resume safely across browsers, and export a trustworthy record.

Deliver:

- selected MVP identity mode, campaign membership authorization, secure browser sessions;
- autosave status, play-session boundaries, turn-audit pagination, durable generation jobs, and recap artifact;
- canonical private export and player-readable export;
- archive/delete flows, encrypted MongoDB backup/isolated restore, document/audit schema fixtures, and correction audits;
- authorized private asset delivery and retention classifications.

Acceptance:

- one user's guessed campaign/character/artifact IDs never reveal or mutate another's data;
- restore from backup loads sampled campaign/character canonical state hashes and complete turn histories;
- an older supported fixture loads after migrations with unchanged mechanical meaning;
- export includes rules/content pins, rolls, and provenance but excludes credentials/private source bodies;
- delete/archive behavior matches the documented retention policy and is verified in a drill.

## Slice 5 — Consented real-life inspiration

**Player value:** an occasional fictional event can echo an opted-in shared memory while everyone retains control.

Deliver:

- feature-flagged private Markdown ingestion, schema/identifier/sensitivity linting, source versioning;
- participant/campaign/media consent records and campaign safety setup;
- deterministic eligibility/weight/cooldown selection and high-fiction-distance text transform;
- pause, veil, veto, category/source disable, revocation, derived-artifact deletion/redaction;
- audit and incident-response runbook using opaque IDs.

Acceptance:

- feature defaults off and a source with missing/revoked/expired consent is statistically and deterministically unselectable;
- source text and real identifiers are absent from client bundles, normal logs, metrics, exports, and provider requests beyond minimized approved facts;
- hostile instructions inside Markdown do not alter policy or invoke capabilities;
- veto immediately hides presentation and continues with an unrelated fallback without demanding a reason;
- a full consent revocation/deletion exercise passes, including pending jobs and documented backup limits.

## Slice 6 — Asynchronous scene images

**Player value:** request a scene illustration and continue playing while it appears.

Deliver:

- environment-selected image-provider adapter and typed `ImageBrief`;
- durable leased job, bounded retry/cancel, placeholder, polling/stream update;
- pre/post safety checks, metadata stripping, content verification, and protected local-storage web variants;
- generated alt text, provenance/config fingerprint, cost/rate limits;
- no-likeness/no-private-source enforcement.

Acceptance:

- the turn completes while generation is queued, slow, rejected, or unavailable;
- refresh/restart does not lose or duplicate the job/artifact;
- unauthorized users cannot fetch the original or variants;
- invalid MIME/oversized/unsafe results are quarantined, not rendered;
- every displayed image has a text alternative and provenance record.

## Slice 7 — MVP release gate

**Player value:** a stable, understandable campaign experience suitable for the intended private test group.

Deliver and verify:

- rules traceability and license/NOTICE review;
- threat model, dependency/license scan, secret scan, penetration-focused authorization tests;
- accessibility/manual browser pass, responsive UI, reduced-motion dice presentation;
- load/soak tests for turn transactions and worker limits; provider chaos tests;
- dashboards/alerts, backup restore, incident/degraded-mode runbooks;
- guided first-run experience, limitations page, feedback/privacy reporting.

Acceptance:

- all MVP requirements and quality gates have linked automated/manual evidence;
- no open critical security, privacy, rules-correctness, data-loss, licensing, or accessibility issue;
- an internal campaign can be created, played, inspired, illustrated, advanced, exported, stopped, and resumed without operator database repair;
- rollback is rehearsed without making newly committed campaigns unreadable.

## Post-MVP waves

1. **Broader SRD 5.1:** expand options/mechanics only in complete capability-tested bundles; progress toward levels 1–20.
2. **Group play:** invitations, per-character control, turn notifications, presence, shared consent, concurrent commands, moderation.
3. **Creator platform:** authoring/validation CLI, signed packs, safe review workflow, distribution policy.
4. **Richer media:** portraits, map generation, voice/accessibility narration, consistency tools, each with separate rights/safety gates.
5. **Alternative rules profile:** assign and implement SRD 5.2.1 as a separate adapter and compendium, then design an explicit conversion report/workflow.
6. **Scale storage:** evolve MongoDB topology and add object storage only after measured transaction, replication, database-size, artifact, or multi-instance limits, using repository contract tests and verified export/restore; keep Dragonfly non-authoritative.

## Major risks and early proofs

| Risk | Earliest proof/mitigation |
| --- | --- |
| AI contradicts mechanics | Slice 1 facts/explanations before AI; Slice 3 typed proposals and fidelity evals |
| Rules scope explodes | Capability matrix; only expose fully implemented character/content combinations |
| Saves break as schemas evolve | Slice 1 document/audit fixtures, canonical load/history checks, independent version axes |
| Hydration leaks or diverges | Slice 0 shared-render determinism and artifact secret scan |
| Personal memories harm trust | Feature off until Slice 5 consent/deletion adversarial tests pass |
| Image latency/cost dominates play | Slice 6 asynchronous optional artifact with budgets and fallback |
| Licensing contaminates content | Provenance required at pack ingestion and release, not retrofitted |
