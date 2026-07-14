# Decision register and open questions

This register prevents unresolved choices from leaking into code as accidental policy. “Accepted” decisions may change only through a recorded replacement with migration/compatibility impact. Open questions have a latest responsible slice.

## Accepted planning decisions

| ID | Decision | Consequence |
| --- | --- | --- |
| ADR-P01 | MVP uses the implemented Leptos 0.8/Axum modular workspace and SQLite/SQLx persistence; PostgreSQL/services are measured scale evolutions. | Preserve `app`/`frontend`/`server` and `game-core`/`game-server` boundaries; add durable SQLite jobs for asynchronous generation. |
| ADR-P02 | Initial mechanics/content profile ID is `srd-5.1-cc`; SRD 5.2.1 is a future separate profile. | Campaigns pin a ruleset; no silent mixing/conversion. |
| ADR-P03 | The deterministic Rust engine is authoritative; AI emits typed proposals/presentation only. | Model/provider failure cannot change or block mechanical truth. |
| ADR-P04 | MVP persists revisioned campaign/character JSON documents plus append-only turn/asset audits in SQLite. | Expected-revision saves and correction audits are required; a complete event stream/snapshots are later work and cannot be claimed yet. |
| ADR-P05 | Text/image providers use independent `TEXT_LLM_*` and `IMAGE_LLM_*` typed profiles loaded with `dotenvy`; MVP applies changes on restart. | Disabled is the safe default; no credentials in client/config fingerprints and no MVP hot reload. |
| ADR-P06 | Domain/boundary errors use dedicated `thiserror` types mapped to stable safe client codes. | Internal causes remain observable without leaking details. |
| ADR-P07 | Real-life inspiration is feature-flagged off by default, requires all-party scoped consent, and uses deterministic eligibility before generation. | Surprise cannot override privacy/safety. |
| ADR-P08 | Private inspiration Markdown lives in a configured protected server source, not a public content pack or client bundle. | Repository examples are synthetic; real files are ignored/encrypted as deployment policy requires. |
| ADR-P09 | Scene images are optional asynchronous artifacts with placeholders and provenance. | A slow/rejected image never blocks a turn or save. |
| ADR-P10 | Theme packs affect presentation/composition; mechanical homebrew is a distinct post-MVP category. | Themes cannot silently override SRD behavior. |
| ADR-P11 | The 2018 Basic Rules PDF is reference-only; distributable rules expression/data comes from the SRD 5.1 CC edition with attribution. | Provenance/release gates reject unlicensed reference material. |
| ADR-P12 | MVP is turn-based and has one campaign owner controlling one hero. | Invited multi-player character ownership/concurrency is post-MVP. |
| ADR-P13 | MVP advancement uses the implemented SRD 5.1 XP thresholds and validated XP awards. | Milestone advancement would be a separately versioned campaign policy later. |

## Unresolved product and policy questions

| ID | Question | Recommended default | Decide by / owner |
| --- | --- | --- | --- |
| Q01 | Is the first deployment local single-user, invite-only hosted, or public registration? | Invite-only hosted if remote access is required; otherwise explicit single-user mode. No public registration. | Slice 0 / Product + Security |
| Q02 | Which exact browsers, devices, accessibility standard, languages, and time zones are supported? | Current stable Firefox/Chromium/Safari desktop/mobile; WCAG 2.2 AA target; English first; store UTC/display local. | Slice 0 / Product + UI |
| Q03 | Which ability-score generation method ships? | Fixed licensed SRD 5.1 method for predictable creation; add audited random generation later. | Before Slice 2 / Game design |
| Q04 | Which SRD 5.1 character options and spell list are fully supported in MVP? | One martial and one spell-using path with levels 1–2, plus a deliberately small spell/equipment set; hide everything else. | Before Slice 1 content model / Game design + Rules |
| Q06 | How lethal is the campaign and what happens at defeat? | Implement profile-correct unconscious/death mechanics, with a campaign safety option for a non-terminal story recovery event that is clearly homebrew. | Before Slice 1 encounter / Game design |
| Q07 | How are GM-proposed DCs bounded and when must players confirm stakes? | Difficulty-band proposal mapped by Rust; confirm irreversible/high-stakes checks before rolling. | Before Slice 3 / Game design |
| Q08 | Which text and image provider adapters ship first, and are private inputs contractually permitted? | Deterministic fake plus one operator-selected provider per modality; do not enable personal inspiration until retention/training/region terms are approved. | Text: Slice 3; image: Slice 6 / Engineering + Privacy |
| Q09 | Are scene images manual, automatic, or both, and what are cost limits? | Manual/on-demand MVP with per-campaign hard cap and owner-visible spend estimate. | Before Slice 6 / Product + Operations |
| Q10 | May players regenerate narration/images, and which versions remain visible/exported? | Allow bounded presentation retries; preserve selection history privately, export only selected versions plus provenance, and never reroll mechanics. | Slice 3 / Product + Privacy |
| Q11 | What exact age restriction and sensitive-topic policy applies? | Adults-only private MVP; no minors in inspiration sources; conservative prohibited categories from safety plan. | Before any external test / Product + Privacy/Legal |
| Q12 | Who administers participant identities/consent, and how is consent verified? | Campaign operator maps pseudonymous participant IDs to explicit out-of-band confirmation; no self-attested third-party consent. | Before Slice 5 / Product + Privacy |
| Q13 | What retention applies to campaigns, encrypted debug prompts, model attempts, audit events, deleted sources, artifacts, and backups? | No full prompt capture by default; short failed-attempt metadata TTL; user-controlled campaign archive; documented finite backup expiry. | Before Slice 4 schema freeze / Privacy + Operations |
| Q14 | Can campaigns or recaps be shared publicly? | No public links in MVP; authenticated private export only. Design a separately redacted share projection later. | Before Slice 4 / Product + Privacy |
| Q15 | What project code license and original content/pack licenses apply? | Keep undecided material private; choose explicit code and original-content licenses before outside contributions or distribution. | Before public repository/release / Maintainer + Legal |
| Q16 | Is “Manchester Arcana” available and appropriate as product/domain branding? | Use it as the current name while conducting name/domain/trademark review and retaining an original visual identity. | Before public release / Product + Legal |
| Q17 | Are model-produced NPCs/locations promoted into reusable packs? | No automatic promotion; require human edit, rights/safety/provenance review, and a new immutable pack version. | Before creator tooling / Content + Legal |
| Q18 | What analytics are acceptable for the private group? | Operational telemetry only, no behavioural tracking or content capture; revisit through explicit consent. | Slice 0 / Product + Privacy |
| Q19 | Should players see RNG seeds/cursors, and when? | Always show full dice/modifier audit; keep raw seed server-side in solo MVP, design commitment/reveal before competitive group claims. | Before Slice 1 persistence / Rules + Security |
| Q20 | What is the support window for old documents/audits/rules/content packs? | Retain read/history support for every campaign created by a released version until an explicit export/migration/end-of-support policy exists. | Before first public release / Engineering + Product |

## Decision process

For each resolved question, record date, owner, decision, alternatives, rationale, affected documents, data/version migration, and test/rollout implications. If a decision changes a durable semantic—rules, event schema, consent, visibility, pack content, or progression—it requires an explicit version and compatibility plan rather than an in-place edit.
