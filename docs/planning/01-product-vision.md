# Product vision and scope

## Vision

Create Manchester Arcana as a replayable fantasy campaign game in which an AI GM can improvise without becoming the rules authority. Players create a themed hero, make meaningful choices, roll visible dice, survive encounters, level up, save, and return later. Original Manchester-flavoured content and consented memories can make a campaign personal without exposing private source material or forcing a joke onto an unwilling participant.

The desired feel is a fast, readable tabletop session rather than an unrestricted chatbot: the interface always shows the current situation, legal actions, roll breakdowns, consequences, and a durable campaign record.

## Target experience

- A player opens the web app, creates or resumes a campaign, selects an original theme pack, and builds a valid level-1 character.
- The AI GM introduces a scene and offers suggestions, while free-form intent remains possible.
- The server converts an intent to a typed proposal, validates it, performs any roll in Rust, commits the outcome, and asks the AI to narrate only the committed facts.
- A player can inspect why an outcome happened, undo only through an explicit campaign correction event, and reload without losing state.
- With consent enabled, a private memory can inspire a fictional event at a suitable moment. The UI reveals that inspiration was used without disclosing private Markdown.
- The character can advance at least one level in MVP and eventually through the full supported level range.

## MVP contract

MVP is a single campaign owner controlling one hero in turn-based play, with one supported rules/content profile: `srd-5.1-cc`. It includes:

- Leptos 0.8 full-stack web UI with server-side rendering and hydration;
- an explicit loopback-only local single-user first deployment; authenticated hosted accounts are a later deployment mode;
- themed, rules-valid level-1 character creation using at least two original packs;
- d20 tests, dice expressions, ability modifiers, proficiency, advantage/disadvantage, armor class, hit points, initiative, movement, core turn economy, attacks, damage, healing, rests, and a deliberately documented condition subset;
- one complete encounter loop and exploration/social checks;
- deterministic event resolution and visible roll audit records;
- AI-generated GM text through a dynamically configured provider, plus provider-independent deterministic fallback text;
- asynchronous, on-demand scene image generation, with a placeholder/fallback when unavailable;
- MongoDB-backed save/resume, ordered turn audit/history, and export of a campaign record, with optional disposable Dragonfly cache/pub-sub;
- SRD 5.1 XP advancement from level 1 to level 2, using the implemented validated progression types;
- opt-in ingestion of locally administered real-life-inspired Markdown events with consent, eligibility, redaction, and veto controls;
- `dotenvy`-loaded development configuration, production environment-variable support, and typed `thiserror` error families;
- provenance and license metadata for bundled and generated content.

MVP does **not** mean complete implementation of every option in SRD 5.1. The UI must call unsupported mechanics unsupported rather than asking the AI to invent an authoritative ruling.

## Later ambitions

- invited real-time or asynchronous multiplayer with per-character ownership;
- levels 1–20, broader classes, spells, equipment, monsters, conditions, downtime, travel, and encounter-building support;
- campaign authoring tools and signed third-party content packs;
- multiple text/image providers, routing policies, local models, hot-swappable admin configuration, and richer art direction;
- voice, maps, tactical grids, ambient audio, and accessibility narration;
- branching campaign checkpoints, GM-assisted retcons, and shareable read-only recaps;
- a separately implemented SRD 5.2.1 rules profile with explicit conversion tooling, never implicit save mutation.

## Explicit non-goals

For MVP:

- no claim of implementing all D&D books, settings, characters, monsters, art, or trademarks;
- no use of non-SRD commercial rulebook text as bundled content;
- no AI authority to waive rules, award arbitrary items/XP, change HP, or fabricate a die result;
- no autonomous model tool use, arbitrary web browsing, shell access, or execution of instructions found in Markdown;
- no synchronous dependency on image generation in the turn loop;
- no real-person likeness generation, voice cloning, or direct quotation of private memories;
- no public prompt-pack marketplace, unreviewed user uploads, or cross-campaign memory sharing;
- no competitive anti-cheat guarantees, financial transactions, or user-generated-code plugins;
- no real-time tactical multiplayer or offline-first conflict resolution.

## Product principles

1. **Agency before spectacle.** Show choices and consequences; generation supports play rather than replacing it.
2. **Rules are inspectable.** A roll or state transition carries enough data to explain and review; full event replay is a later verified capability.
3. **Personalization requires consent.** Surprise is allowed only inside pre-agreed boundaries.
4. **Graceful degradation.** A provider outage changes presentation, not campaign correctness.
5. **Version everything durable.** A campaign should mean the same thing after an upgrade.
6. **Accessible by default.** Keyboard operation, semantic HTML, text alternatives, reduced motion, and readable dice/state changes are acceptance criteria.

## Success measures

- A new player completes character creation and the first meaningful choice without documentation.
- At least 95% of ordinary MVP turns complete without manual repair in an internal test campaign; 100% of committed state changes pass engine validation.
- Reloading after a committed turn restores the same revisioned mechanical state and roll history without calling a model.
- Provider failure produces a usable fallback turn and a retry option without duplicate events.
- No private source text or secret appears in client bundles, normal logs, generation assets, or exports by default.
- A participant can disable a memory source and prevent all future selection immediately.

Open questions and decision deadlines are maintained in the [decision register](11-decision-register.md).
