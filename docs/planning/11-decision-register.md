# Decision register and open questions

This register prevents unresolved choices from leaking into code as accidental policy. “Accepted” decisions may change only through a recorded replacement with migration/compatibility impact. Open questions have a latest responsible slice.

## Accepted planning decisions

| ID | Decision | Consequence |
| --- | --- | --- |
| ADR-P01 | The implemented Leptos 0.8/Axum modular workspace remains the MVP baseline; its original embedded-database choice is superseded by ADR-P15. | Preserve `app`/`frontend`/`server` and `game-core`/`game-server` boundaries. |
| ADR-P02 | Initial mechanics/content profile ID is `srd-5.1-cc`; SRD 5.2.1 is a future separate profile. | Campaigns pin a ruleset; no silent mixing/conversion. |
| ADR-P03 | The deterministic Rust engine is authoritative; AI emits typed proposals/presentation only. | Model/provider failure cannot change or block mechanical truth. |
| ADR-P04 | MVP persists revisioned campaign/character BSON documents plus append-only turn/asset audits in MongoDB. | Expected-revision saves and correction audits are required; a complete event stream/snapshots are later work and cannot be claimed yet. |
| ADR-P05 | Text/image providers use independent `TEXT_LLM_*` and `IMAGE_LLM_*` typed profiles loaded with `dotenvy`; MVP applies changes on restart. | Disabled is the safe default; no credentials in client/config fingerprints and no MVP hot reload. |
| ADR-P06 | Domain/boundary errors use dedicated `thiserror` types mapped to stable safe client codes. | Internal causes remain observable without leaking details. |
| ADR-P07 | Real-life inspiration is feature-flagged off by default, requires all-party scoped consent, and uses deterministic eligibility before generation. | Surprise cannot override privacy/safety. |
| ADR-P08 | Private inspiration Markdown lives in a configured protected server source, not a public content pack or client bundle. | Repository examples are synthetic; real files are ignored/encrypted as deployment policy requires. |
| ADR-P09 | Scene images are optional asynchronous artifacts with placeholders and provenance. | A slow/rejected image never blocks a turn or save. |
| ADR-P10 | Theme packs affect presentation/composition; mechanical homebrew is a distinct post-MVP category. | Themes cannot silently override SRD behavior. |
| ADR-P11 | The 2018 Basic Rules PDF is reference-only; distributable rules expression/data comes from the SRD 5.1 CC edition with attribution. | Provenance/release gates reject unlicensed reference material. |
| ADR-P12 | MVP is turn-based and has one campaign owner controlling one hero. | Invited multi-player character ownership/concurrency is post-MVP. |
| ADR-P13 | MVP advancement uses the implemented SRD 5.1 XP thresholds and validated XP awards. | Milestone advancement would be a separately versioned campaign policy later. |
| ADR-P14 | On 2026-07-14, the first implemented deployment was fixed as explicit local single-user mode. | Bind only to loopback HTTP, enforce matching loopback browser Host/Origin, and fail hosted startup until authenticated sessions and campaign authorization exist; this does not protect against another local process. |
| ADR-P15 | On 2026-07-24, MongoDB became the only authoritative application database and DragonflyDB an optional disposable cache/pub-sub layer. | Use replica-set transactions, managed validators/indexes, revision/idempotency checks, isolated live-database tests, separate app/schema credentials, encrypted Mongo recovery archives, and no dual-backend/import abstraction. |

## Resolved private-MVP product and policy questions

Q02–Q20 were accepted for the private MVP on 2026-07-14 in the
[full resolution record](12-mvp-policy-resolutions.md). The record contains the exact
scope, alternatives, rationale, durable-version impact, tests, and rollout constraints.

Q08 real-provider privacy approval and Q15/Q16 public-distribution clearance remain
explicit external-release holds. They do not block a local/private working game: the
implementation uses deterministic fake or disabled providers and a private working
title. Release configuration must fail closed until separate approval records exist.

## Decision process

For each resolved question, record date, owner, decision, alternatives, rationale, affected documents, data/version migration, and test/rollout implications. If a decision changes a durable semantic—rules, event schema, consent, visibility, pack content, or progression—it requires an explicit version and compatibility plan rather than an in-place edit.
