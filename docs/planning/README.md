# Manchester Arcana planning index

Status: working product and engineering plan. Decisions marked **MVP** are the current delivery contract; later items are direction, not promises. The full-stack walking-skeleton foundation and Slice 1A persisted exploration check are implemented; remaining Slice 0 acceptance gates and the initiative/combat/damage/HP portion of Slice 1 are pending.

Manchester Arcana is a web-based, AI-GM fantasy role-playing game. It uses a deterministic Rust rules engine based on the 2014-era SRD 5.1 while generative models provide prose, scene ideas, and optional images. The AI proposes; the engine decides.

## Documents

1. [Product vision and scope](01-product-vision.md) — audience, MVP boundary, non-goals, success measures, and open questions.
2. [System architecture](02-architecture.md) — crate boundaries, Leptos 0.8 SSR/hydration, server APIs, configuration, and errors.
3. [Rules and gameplay](03-rules-and-gameplay.md) — SRD 5.1 coverage, authoritative resolution, rolls, actions, combat, and advancement.
4. [AI generation](04-ai-generation.md) — provider abstraction, structured proposals, text/image flows, fallbacks, and cost controls.
5. [Persistence](05-persistence.md) — MongoDB documents/audits, transactions, save/resume, schema bundles, Dragonfly degradation, versioning, and recovery.
6. [Consent, privacy, and safety](06-consent-privacy-safety.md) — safe use of real-life-inspired Markdown prompts and player controls.
7. [Characters and content packs](07-characters-and-content-packs.md) — themed creation and independently versioned content.
8. [Delivery roadmap](08-delivery-roadmap.md) — vertical slices and acceptance criteria.
9. [Quality, observability, and security](09-quality-observability-security.md) — verification strategy and operational gates.
10. [Licensing and provenance](10-licensing-and-provenance.md) — SRD use, attribution, trademarks, and generated-asset provenance.
11. [Decision register](11-decision-register.md) — accepted architectural decisions and resolution status.
12. [MVP policy resolutions](12-mvp-policy-resolutions.md) — exact private-MVP product, safety, retention, and release choices.

## Architectural invariants

- `srd-5.1-cc` is the initial and only MVP ruleset ID. SRD 5.2.1 is a possible future, separate ruleset with separate saves and conformance tests.
- The first deployment is explicit local single-user mode: loopback HTTP only, with matching `Host`/`Origin` enforcement. Hosted mode fails startup until authentication and campaign authorization are implemented.
- Every state change is validated and committed by the deterministic Rust engine. Neither a browser nor a model can set authoritative game state directly.
- A saved campaign pins rules, content, prompt, and payload-schema versions. Loading or showing saved history never calls a model; full event replay is a later evolution.
- Model credentials, private prompt sources, consent records, and full campaign state remain server-side.
- Generated content is optional. Provider failure degrades to deterministic templates and never blocks saving or rules resolution.
- Real-life-inspired material is opt-in, attributable to a private source ID, revocable, and subject to campaign safety settings before selection.
- Bundled rules expression comes from SRD 5.1 under CC BY 4.0. The linked 2018 Basic Rules PDF is a design reference only.

## Primary references

- [Leptos book](https://book.leptos.dev/), including [SSR](https://book.leptos.dev/ssr/index.html), [SSR modes](https://book.leptos.dev/ssr/23_ssr_modes.html), [hydration pitfalls](https://book.leptos.dev/ssr/24_hydration_bugs.html), and [server functions](https://book.leptos.dev/server/25_server_functions.html)
- [2018 Basic Rules PDF](https://media.wizards.com/2018/dnd/downloads/DnD_BasicRules_2018.pdf) — reference only; do not copy or bundle from this file
- [Official SRD downloads and FAQs](https://www.dndbeyond.com/srd) — use the SRD 5.1 Creative Commons download for the initial ruleset
- [CC BY 4.0 deed](https://creativecommons.org/licenses/by/4.0/) and [legal code](https://creativecommons.org/licenses/by/4.0/legalcode)
- [`dotenvy` documentation](https://docs.rs/dotenvy/latest/dotenvy/) and [`thiserror` documentation](https://docs.rs/thiserror/latest/thiserror/)
