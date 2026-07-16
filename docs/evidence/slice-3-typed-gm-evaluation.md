# Slice 3 typed-GM evaluation evidence

Date: 2026-07-15

Status: the typed proposal/prompt boundary and the durable narration-presentation sub-slice pass deterministic component, PostgreSQL, and browser evidence. This remains bounded evidence, not complete Slice 3 or MVP acceptance.

## Reproducible command

```text
cargo test -p manchester-dnd-server --test typed_gm_evaluation --no-fail-fast
```

Result: 3 integration tests passed. The versioned corpus test executed all 16 cases in [`tests/fixtures/typed-gm/v2/cases.json`](../../tests/fixtures/typed-gm/v2/cases.json) and matched [`typed-gm-v2.json`](typed-gm-v2.json).

## Covered boundary behavior

| Area | Executable evidence |
| --- | --- |
| Valid typed proposals | Action, scene, high-stakes check, clarification, and exact-claim narration candidates validate through the public service/core APIs. |
| Schema and allowlists | Unknown fields and unsupported schema versions fall back as malformed; an invented action ID falls back as unsafe. |
| High stakes | A `character_defeat` check produces `RequirePlayerConfirmation`, never an engine command disposition. |
| Ambiguity | A bounded two-choice proposal produces `AskClarification`. |
| Prompt injection and minimization | Hostile intent remains inside explicit untrusted-data markers. The serialized envelope has no field for hidden state, raw/private source bodies, credentials, or dice seeds. Hostile provider prose falls back. |
| Provider degradation | Disabled, fake, timeout, unavailable/outage, rate-limit, malformed, unsafe, and contradictory outputs have deterministic expected outcomes. |
| Narration facts | Accepted and fallback narration carries a claim set exactly equal to the committed `MechanicalFact` set; altered claims fall back as contradiction. |
| Mechanical isolation | Two presentation requests, each including two rejected provider attempts, leave the deterministic RNG cursor, next die result, input revision, event sequence, and authoritative facts unchanged. |

The 16-case result is 7 provider acceptances, 9 authored fallbacks, 16 core-valid authority/fact checks, and zero unsafe outputs accepted. Every configured failure class is observed at least once. The machine evidence requires 1,000,000 parts-per-million structured fact fidelity and zero escaped unsafe outputs.

## Durable narration presentation evidence

Reproducible commands run on 2026-07-14:

```text
scripts/validate-migrations.sh --static-only
cargo test -p manchester-dnd-server repository::jobs::tests --lib
cargo test -p manchester-dnd-server repository::presentations::tests --lib
cargo test -p manchester-dnd-server generation_ledger::tests --lib
cargo clippy -p manchester-dnd-server --all-targets -- -D warnings
cargo check -p manchester-dnd-app --features ssr --all-targets
cargo check -p manchester-dnd-app --features hydrate --target wasm32-unknown-unknown
cargo leptos build --release
npm run test:browser:slice3
```

Results: 11 ordered migrations passed static validation; all 11 were also exercised by fresh-schema SQLx tests. The focused presentation suite passed 5 tests, the inline generation-ledger suite passed 3 tests, the strict server clippy gate and both app feature builds passed, and the dedicated Chromium journey passed 1 test.

| Area | Executable assertion |
| --- | --- |
| Atomic provenance | Provider success and safety-validated authored/engine fallback complete the leased job and attempt in the same transaction that selects the presentation. The canonical output digest is copied to job, attempt, and presentation. |
| Stored boundary | The presentation table contains only a bounded, trimmed, deterministic-safety-filtered player-visible body plus campaign/turn, exact job/attempt IDs, source, version/selection timestamps, and config/prompt/policy/output digests. It has no prompt text, player intent, raw provider body, or credential field. |
| Versions and retention | A committed turn has at most three campaign-lifetime version receipts: the initial presentation plus two regenerations. Replacing the selected body gives prior bodies a 30-day deletion timestamp; selected narration has no deletion timestamp. Body cleanup is bounded and idempotent, while the body-free alias remains and returns a stable terminal expiry instead of spending a new version. |
| Concurrency and restart | Concurrent workers serialize version allocation on the immutable turn audit. Exact-key body, evidence, and retained history are read under the same turn lock, so a concurrent later selection cannot be substituted for the requested version. Repository reconstruction retains one selection and the database rejects a fourth version. |
| Exact response replay | A unique campaign/turn/client idempotency alias resolves before current prompt-policy, config, or version-cap preparation. The regression fixtures recover the original row and digests after policy drift, accept a fresh server-generated presentation UUID for an already completed exact attempt, and prove replay does not spend a version. |
| Typed-command recovery | After interpretation and before mechanics, a schema-versioned body-free receipt stores the normalized player-intent digest, exact validated `EncounterIntent`, revisions, interpretation label, and evidence. A retry consults it before stale-revision checks and re-enters the canonical command-receipt path, reconstructing the committed outcome without another provider call or dice resolution; different text under the same key conflicts. |
| Unsafe output | Control characters, HTML-like tags, script markers, prompt-exfiltration markers, and oversized bodies fail closed before storage. External free-form prose is not admitted by the current deployment path. |
| Playable browser flow | A fresh-database deterministic-fake journey creates a hero, lets the server commit the initial typed mechanic, and deliberately discards that response. The retained exact command reconstructs the committed result and version 1. The test then discards the committed version-2 response, recovers exact version 2 without creating version 3, selects version 3, and observes the disabled retry cap. |
| Mechanical isolation | Before and after both presentation-only retries, the browser compares encounter metadata, combatants, legal actions, and every rendered roll record for exact equality. No encounter command, RNG draw, HP, XP, or campaign/encounter revision is rerun by regeneration. |
| Reactive lifetime | The free-form result/retry signal bundle is owned by the stable encounter panel, so updating the authoritative campaign view after a commit no longer remounts away narration history or the interrupted-response key. |

## Honest residual gaps

- The synthetic corpus itself does not exercise the browser or persistence; the separate deterministic-fake browser and PostgreSQL suites above cover that boundary without making a live-provider claim.
- The provider execution remains inline. Durable job/lease primitives and repository reconstruction are tested, but no background narration worker recovers an in-flight browser request across a server-process crash.
- A process failure after provider interpretation completes but before the pending typed-command receipt inserts fails closed. No mechanics have run in that window, but the completed metadata-only provider attempt cannot currently reconstruct its proposal. Once the pending receipt exists, every tested crash/response-loss boundary resumes through the canonical mechanics receipt without rerolling.
- There is no per-turn, per-campaign, or per-provider monetary budget enforcement. `cost_microusd` remains unavailable, and concurrent callers can spend provider work before the transactional three-version cap rejects an extra stored version.
- External prose remains fail-closed until an independent moderation and prose-to-fact semantic-entailment boundary exists. The current OpenAI-compatible path records safe engine-authored narration rather than storing external prose.
- The suites use deterministic synthetic, fake, and disabled adapters. They make no live-provider quality, latency, training-use, region, moderation, terms, or cost claim.
- High-stakes confirmation is proven only as a typed disposition. Confirmation UI, authorization, durable confirmation state, and subsequent command execution still need end-to-end evidence.
- Exact equality is enforced for structured `claimed_facts`; there is no independent semantic entailment checker proving arbitrary prose agrees with those claims. Provider prose that lies while returning an exact claim set remains a residual fidelity risk.
- Prompt minimization is proven for the typed request envelope, not source ingestion, consent eligibility, revocation, deletion, or provider-side handling.
- Version visibility is local-campaign scoped; durable multi-user identity, ownership authorization, and export/delete controls remain Slice 4/5 work.

No checklist item is marked complete by this evidence alone.
