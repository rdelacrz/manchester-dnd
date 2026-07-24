# Game Mechanics and AI Runtime State

## Purpose

The user's brief describes the core loop but leaves many durable D&D mechanics unspecified. This document defines what the MongoDB design must be able to represent. It is a **storage and authority plan**, not a commitment to implement every optional D&D rule in the first release.

The current code already provides valuable deterministic foundations in `crates/game-core/`, including:

- intent-only commands and server-owned dice/modifiers;
- ability checks and saves;
- initiative, attacks, damage, temporary HP, death saves, conditions, movement, and action economy;
- short/long rests, hit dice, selected class resources and spells;
- character source choices plus derived sheets;
- encounter-start character snapshots;
- rewards and exact command receipts;
- exploration/social checks;
- typed GM proposals based on revisions/event sequence and legal action IDs;
- private-inspiration filtering, consent, cooldown, and minimized prompts.

Those invariants should survive the persistence rewrite. Current content limitations—principally Fighter/Wizard, levels 1–2, one authored encounter, and narrow spell/action sets—must not become MongoDB constraints.

## Ruleset decision

D&D SRD v5.1 and SRD v5.2.1 differ in character creation, classes, spells, actions, conditions, and terminology. Do not mix them inside one unversioned model.

Recommended sequencing:

1. Keep current deterministic mechanics under an explicit legacy/current project ruleset ID while replacing persistence.
2. Add SRD v5.2.1 as a new, versioned content/rules pack after the MongoDB vertical slices are stable.
3. Let each campaign pin exactly one ruleset/content bundle at activation.
4. Reject any source/template whose ruleset is incompatible with the campaign.

If the product chooses SRD v5.2.1 immediately, make that a separate rules-engine workstream with its own conformance tests rather than silently changing behavior during repository replacement.

Official reference: D&D SRD v5.2.1, <https://www.dndbeyond.com/srd>.

## Authority hierarchy

From most to least authoritative:

1. **Pinned deterministic rules/content** and server code.
2. **Current MongoDB aggregate revisions**.
3. **Committed `turn_events`, receipts, audits, and ledger entries**.
4. **Validated typed GM proposal** awaiting a commit.
5. **Player-selected action ID or player-authored custom text**.
6. **Raw LLM output**, which is never authoritative.

The AI GM may narrate, select among server-provided legal actions, and propose typed outcomes. It may not directly set dice, modifiers, DC, HP, money, XP, inventory, conditions, encounter state, rewards, or BDE.

## Character library versus campaign runtime

### Level-less `player_characters`

A reusable character records identity and a build blueprint:

- display name, pronouns, description, portrait;
- ruleset/content identity;
- species/ancestry and lineage options;
- background/origin and granted proficiencies/features;
- starting class and intended subclass;
- ability-score generation method and assignments;
- skill/tool/weapon/armor/language proficiency choices;
- starting equipment choices;
- cantrip/spell choices where character creation requires them;
- feat and future build choices;
- player-facing backstory, traits, ideals, bonds, and flaws if desired;
- bounded custom traits that do not claim mechanical authority.

It does **not** record level, XP, current/max HP, hit dice, spell slots, prepared spells as a runtime list, current resources, inventory mutations, currency, conditions, death saves, encounter position, exhaustion, or BDE.

A planned subclass may be stored even if the ruleset does not activate it until a later level. Active features are derived only when the campaign instance reaches the required level.

### `campaign_character_instances`

A campaign-specific instance must represent:

#### Progression

- current level and XP or milestone count;
- one explicit campaign progression policy;
- class progression by level, supporting future multiclassing without a schema rewrite;
- subclass activation and level-specific choices;
- feats/ability-score improvements;
- HP gain decisions/roll facts;
- learned/replaced spells and invocations/techniques;
- advancement history with source event/receipt IDs;
- derived sheet version/digest.

#### Core statistics

- six ability scores/modifiers;
- proficiency bonus;
- saving-throw proficiencies and modifiers;
- skill proficiencies/expertise and modifiers;
- passive values;
- armor class and its derivation;
- speed modes (walk, climb, swim, fly, burrow) and temporary changes;
- size, senses, languages, resistances/immunities where applicable;
- attacks, save DCs, and legal action capabilities;
- class/species/background/subclass/feat features.

#### Survivability and effects

- maximum/current/temporary HP;
- death-save successes/failures and life status;
- hit dice by die type, spent/available;
- conditions with source, start, duration, expiry trigger, save/end rule, and stacking policy;
- exhaustion level according to pinned ruleset;
- ongoing effects, concentration, wards, and temporary modifiers;
- recovery policy and rest history where exact-once enforcement needs it.

#### Class and spell resources

- generic named resource pools with current, maximum, reset trigger, and source feature;
- spell slots by level;
- spells known, prepared, and spellbook where the class requires distinctions;
- concentration and current summoned/persistent spell effects;
- action surge, second wind, rage, sorcery points, channel resources, superiority dice, ki/focus, wild shape, bardic inspiration, and future resources as content-defined pool IDs rather than database columns.

#### Inventory and economy

- stackable inventory items and unique item instances;
- equipped/held/worn state;
- attunement and capacity;
- ammunition, charges, durability only where rules/content uses them;
- currency denominations and canonical total/value policy;
- quest items and provenance;
- carrying capacity/encumbrance policy pin;
- loot/reward transaction history via turn events/receipts rather than an embedded history array;
- current BDE materialized balance plus lifetime counters, backed by `bde_ledger`.

#### Campaign identity

- source library character ID/revision/digest and immutable snapshot;
- campaign and controlling account IDs;
- active/retired/dead state;
- public-party versus owner/GM-hidden projection policy;
- revision and timestamps.

## Character creation and advancement gaps

The implementation must make explicit decisions for:

- rules edition/content license and available classes/subclasses/species/backgrounds;
- ability-score method: standard array, point buy, rolled, or campaign policy;
- starting level and equipment/gold method;
- fixed-average versus rolled HP;
- XP versus milestone advancement;
- multiclass prerequisites and progression;
- feats/optional rules;
- spell selection/preparation replacement rules;
- respec/rebuild policy;
- character death, retirement, resurrection, and replacement;
- whether a reusable character can have more than one active instance in the same campaign (recommended: no) or across campaigns (recommended: yes).

Every choice that affects mechanics must be explicit and versioned. Do not store only a final number when replay/conformance needs the source choices.

## Campaign lifecycle and snapshots

Recommended campaign lifecycle:

```text
draft -> open -> archived
```

Play activity is separate:

```text
waiting play session -> active -> closed
```

An `open` campaign can have at most one waiting/active play session. `archived` prevents new sessions but keeps history readable. Deletion is an explicit destructive operation and is blocked while a play session or encounter is open.

### Sealing

On first play-session activation, seal:

- ruleset/content pack IDs, versions, and digests;
- progression, lethality, rest, encumbrance, and BDE policies;
- AI GM prompt/policy/config fingerprints;
- event registry/version and safety policy;
- campaign member/character source snapshot basis.

Later library/template edits never mutate sealed runtime state.

### Character and enemy snapshots

- Creating a campaign character instance snapshots the level-less character and chosen rules/content basis.
- Starting an encounter snapshots combat-relevant character/enemy rules at that revision.
- An encounter continues from its snapshots even if the campaign character levels up later.
- On encounter completion, validated HP/resource/inventory/condition changes synchronize to campaign runtime in the same authoritative transaction as rewards and completion.

## Player membership and control

The data model supports multiplayer even if the first UI is small:

- campaign owner is an active `game_master` member;
- players accept invitations and select/create one campaign character instance;
- play-session participants snapshot the active roster;
- a participant slot is `human_active`, `ai_substitute`, `absent`, `left`, or `defeated`;
- AI substitution is opt-in by campaign/player policy and cannot spend human BDE or make irreversible progression/inventory choices;
- human return uses an audited handoff revision;
- only the active authorized participant may submit a player action;
- party-vote/group-choice policy must be explicit for exploration choices aimed at the whole group.

Recommended initial party maximum is configurable but bounded (for example 6); MongoDB validators use a hard safety ceiling. Do not repeat the current SQL allowance of 64 as the default product party size without a UX/concurrency decision.

## Turn state machine

A durable play session should never be represented as free-form chat alone.

### Modes

Required top-level modes:

```text
exploration
battle
```

Social interaction, travel, investigation, traps, puzzles, downtime transitions, and event scenes are exploration subtypes rather than additional top-level modes initially.

### Generic phases

```text
gm_generation
awaiting_group_or_player_choice
awaiting_custom_action_interpretation
resolving
awaiting_reaction
committing
transitioning_mode
closed
```

The current phase carries:

- event sequence and aggregate revision;
- active account/character/combatant where applicable;
- legal action IDs and their canonical set digest;
- GM proposal/presentation reference;
- timeout/AI-substitution policy;
- active encounter/event reference;
- based-on revision/event sequence;
- correlation and idempotency identity.

A committed turn produces one or more `turn_events` and advances state once. Presentation may be regenerated/versioned without changing mechanics, but selected text is linked to the committed event.

## Exploration mechanics

The exploration model should support:

- locations/zones and transitions;
- public scene facts and GM-hidden facts;
- interactable entities/objects;
- objectives/quests and progress flags;
- time passage and rest eligibility;
- travel pace/environment/hazards if enabled;
- ability checks, skill selection, proficiency, advantage/disadvantage, DC, and stakes;
- group checks, contests, passive checks, and repeated-attempt policy;
- social attitudes/goals and interaction outcomes;
- inventory/resource use;
- event eligibility/cooldown;
- transition into battle with persisted consequences;
- party-wide choice versus active-character choice.

Player commands are intent-only. For example, the browser sends `action_id=inspect_runes`; the server maps it to Intelligence/Arcana, proficiency, DC, stakes, legal effects, and deterministic RNG under the pinned content.

### Random exploration outcomes

Random outcome storage includes:

- eligible outcome set/content digest;
- RNG algorithm/version and seed reference/commitment;
- raw rolls and modifiers;
- selected outcome ID;
- resulting typed effects;
- state revisions before/after.

The AI may narrate the selected outcome but cannot choose an outcome after seeing the roll unless the deterministic rules explicitly provide that choice.

## Battle/encounter mechanics

The encounter aggregate must be general enough for multiple heroes and enemies.

### Combat roster

Each bounded combatant entry includes:

- combatant ID and kind (`hero`, `enemy`, optional NPC/companion);
- campaign character/enemy instance reference;
- immutable encounter-start source snapshot/digest;
- display name and reveal/visibility state;
- AC, speed, ability/save/skill data needed in combat;
- current/max/temp HP and life status;
- death saves for eligible characters;
- position/zone and movement state;
- resources, conditions/effects, concentration;
- attacks/actions/reactions/bonus actions/spell capabilities;
- controlling account/AI/GM policy;
- initiative value and tie-break evidence.

### Encounter state

Persist:

- status `ready|active|victory|defeat|escaped|aborted|completed`;
- rules/content/lethality snapshots;
- map or abstract zones, objects, ranges, and hazards;
- initiative order, round, current actor/index;
- per-turn action, bonus action, reaction, object interaction, and movement;
- pending reaction/decision window;
- objectives and reward eligibility;
- active effects with duration/trigger;
- transition back to exploration;
- exact encounter revision.

### Legal battle actions

At minimum:

- move/dash/disengage/dodge/help/hide/ready/search/study/influence/utilize where supported by the selected ruleset;
- attack and multiattack;
- cast spell and concentration handling;
- class/feature actions, bonus actions, and reactions;
- item use and object interaction;
- grapple/shove or edition-specific equivalents;
- end turn;
- death save;
- flee/surrender/context actions;
- short/long rests outside active hostile combat, not as arbitrary in-combat buttons.

The current code already models a subset (`Move`, `Attack`, `ContextAction`, selected spells, `SecondWind`, `ActionSurge`, rests, death save, `EndTurn`). Extend the typed enum/content registry; do not replace it with unrestricted strings in MongoDB.

### Damage, healing, and conditions

A turn event records:

- attack/check/save roll facts;
- damage/healing components and damage types;
- critical/resistance/vulnerability/immunity calculations;
- temp HP application;
- concentration/death-save consequences;
- condition/effect add/remove/expiry;
- resource and ammunition changes;
- final HP/life state.

Store computed facts for replay/audit but recompute/validate them from rules before write. Never accept computed totals from the browser or LLM.

### Encounter completion

Completion transaction:

1. Validate terminal state and exact encounter revision.
2. Mark reward eligibility/claims exactly once.
3. Synchronize hero HP/resources/conditions/inventory into campaign instances.
4. Retire/retain campaign enemy instances according to outcome.
5. Award XP/milestones, loot, currency, and BDE through typed effects and ledgers.
6. Update play-session mode/turn state back to exploration or close it.
7. Insert turn/audit/receipt records atomically.

## Enemy definitions and generation

### Permission interpretation

- Global `enemy_templates`: admin create/update/archive/list.
- Campaign owners: instantiate approved templates, request AI-generated enemy proposals for owned campaigns, and control campaign-scoped hidden/runtime state.
- AI-generated templates are drafts until deterministic validation and admin/owner approval according to policy. An LLM cannot invent executable rule fields outside registered capabilities.

### Enemy template completeness

Support:

- name, public description, hidden GM notes, image;
- ruleset, size, creature type/tags, alignment if used;
- challenge rating/level and XP reward;
- proficiency bonus;
- AC derivation/type;
- average/formula HP;
- speeds;
- six ability scores;
- saving throws and skills;
- damage vulnerabilities/resistances/immunities;
- condition immunities;
- senses and languages;
- passive perception;
- traits;
- attacks/actions, bonus actions, reactions;
- spellcasting;
- recharge, legendary and lair actions where applicable;
- encounter role/behavior tags and retreat/morale policy;
- loot/reward table reference;
- safety/sensitivity tags;
- version/revision/digest.

### Generated enemies

The AI receives a bounded design brief and allowed mechanic IDs. It returns a typed proposal. The server verifies:

- schema and ruleset/content compatibility;
- every action/effect exists and has legal ranges;
- derived AC/HP/attack/DC/XP/CR constraints;
- no unsafe/forbidden output;
- document and prompt size limits;
- source revision/prompt/config fingerprint.

Only a validated proposal becomes a versioned template or campaign instance. Store provider/model/usage/fingerprints as provenance, not authority.

## Event definitions and resolution

### Admin-managed event template

An event template needs:

- logical ID/revision/digest and ruleset/content pins;
- title and bounded description/prompt facts;
- enabled/draft/archived state;
- eligibility: campaign mode, location/tags, party-level range, required/forbidden flags, enemy/objective state;
- weight and deterministic selection policy;
- cooldown turns and once-per-campaign/session flags;
- sensitivity tags, lines/veils compatibility, participant exclusions;
- predefined choices and/or typed GM-generation purpose;
- allowed effect intents, checks, stakes, rewards, penalties, encounter transition, and follow-up event IDs;
- optional image-generation purpose/template;
- safe authored fallback.

### Campaign event instance

Snapshot the template at selection. Persist status:

```text
selected -> presented -> awaiting_choice -> resolving -> resolved|cancelled
```

Persist eligible-set digest, RNG facts, source snapshot, selected presentation, player/group choice, typed effect resolution, cooldown update, and turn references.

Current `events.rs` privacy safeguards remain: ordinary game storage receives only bounded neutral runtime facts, not raw private Markdown, filenames, identifying source material, or review prose.

## AI Game Master coordination

### Configuration

The provider/model comes from deployment configuration (environment/secret-backed config), not campaign documents. MongoDB may store only:

- non-secret provider config fingerprint;
- provider and model identifier used;
- prompt/policy/content fingerprints;
- attempt count, latency, token usage, finish reason, failure class;
- request/proposal fingerprints;
- selected validated presentation/proposal.

Never persist API keys or full hidden system prompts. Keep stable prompt templates source-controlled and identified by digest.

### GM turn contract

For each GM turn:

1. Build a minimized public/authorized fact set from current revisions.
2. Derive legal action IDs and transition/effect capabilities in deterministic code.
3. Call the AI outside a DB transaction with strict deadlines and size/token budgets.
4. Parse a closed typed schema.
5. Reject unknown fields/actions, stale bases, fact contradictions, unsafe output, leakage, or impossible mechanics.
6. Use an authored deterministic fallback on timeout, rate limit, malformed/unsafe output, or circuit-open state.
7. Commit accepted typed proposal and selected presentation only if revisions/event sequence are unchanged.
8. Record provenance/failure class and advance the state once.

The current `TypedGmTurnInput`, `ProposalAcceptanceContext`, legal-action allowlist, fingerprints, bounded retries, and authored fallback are the model to preserve.

### Predetermined choices

The AI may write labels/dialogue, but each choice maps to a server-issued opaque action ID bound to the current legal-action-set digest. A stale or altered choice fails with conflict; the browser cannot submit arbitrary effect parameters.

### Custom BDE action

Custom player text is a proposal input, not an executable command.

1. Verify active actor, mode/phase, current revisions, sufficient displayed BDE, input size, and safety policy.
2. Interpret against a constrained capability schema and current context.
3. If non-applicable, unsafe, impossible, or stale, reject without charge and optionally present safe alternatives.
4. If applicable, deterministic code determines check/DC/cost/effects/random branches.
5. Commit mechanics and BDE spend atomically as specified in `02-security-consistency-and-operations.md`.
6. Store player-visible custom text only if campaign history requires it; use protected/encrypted storage with retention. Never include it in generic audit logs.

## BDE policy requirements

BDE is a custom system and needs product rules before implementation:

- starting amount;
- maximum balance and overflow behavior;
- custom action cost or variable-cost schedule;
- earning triggers: encounters, events, roleplay, failures, admin grants;
- whether grants are individual or party-wide;
- refund behavior on cancellation after accepted interpretation;
- whether BDE can modify a legal roll, create a new legal action, influence narration only, or any combination;
- whether it survives campaign retirement/death;
- whether the balance is player-visible at all times;
- anti-abuse/rate limits.

Recommended default: integer, campaign-character scoped, bounded, no transfer, no AI-substitute spend, cost charged only in the same transaction as a successful committed custom action, all changes ledgered.

## Safety and private inspiration

Retain the existing project's unusually strong privacy model:

- campaign lines, veils, excluded topics/participants, allowed sensitivities, and kill switch;
- verified participant consent grants and revocation;
- source review and immutable digest/version;
- only minimized neutral facts enter ordinary game storage/provider prompts;
- deterministic eligible-set digest, selection/cooldown evidence, and veto checks;
- high fictional-distance transform policy;
- provider output validation for forbidden identifiers;
- rapid cancellation/deletion and crypto erasure;
- private-source bodies and identifying media outside ordinary application storage/logging.

This is not optional “extra schema”: events and AI generation can create privacy/safety harm without it.

## Presentation and media

Separate mechanics from presentation:

- `turn_events`: mechanics, choices, outcomes, revisions, safe summary/reference;
- `generated_presentations`: versioned narration/dialogue/recap body with audience and selected state;
- `generated_assets`: image/media metadata and authorized owner/campaign/entity association;
- object storage: actual bytes.

Regenerating prose or an image never rewrites committed mechanics. An asset is served only after resolving the parent entity and checking account/campaign access.

## Mechanics implementation tiers

### Tier 0 — Persistence parity foundation

Preserve current deterministic tests and supported mechanics while moving repositories. No rules expansion in this tier.

### Tier 1 — Product brief complete

- gated auth/accounts/roles;
- character library and campaign instances;
- campaign CRUD/membership/delete guard;
- admin enemy/event templates;
- exploration/battle state machine;
- generated enemies/images;
- GM choices and BDE custom actions;
- snapshots, revisions, receipts, ledgers, audits.

### Tier 2 — D&D baseline completeness

- levels supported by selected ruleset;
- broader classes/subclasses/species/backgrounds;
- complete action economy, common conditions, rests, death, inventory/equipment, spell state;
- multi-combatant encounters;
- XP/CR/reward and encounter-balancing tools;
- richer exploration/social/group checks.

### Tier 3 — Optional/advanced mechanics

- multiclassing, feats, complex summons/companions;
- legendary/lair actions and mass encounters;
- travel/downtime/crafting/encumbrance variants;
- grid maps/areas of effect;
- homebrew content authoring with validation/review;
- cross-campaign character import/rebase policy.

Each tier adds content/rules contracts and tests; it should not require re-partitioning the core collections.

## Required mechanics tests

1. Level-less library characters cannot acquire campaign runtime fields.
2. Two campaigns created from one source character diverge independently.
3. Source/template updates do not alter active instance/encounter snapshots.
4. Derived sheets equal pinned choices/content; invalid client-derived totals are rejected.
5. HP/resources/conditions stay within rules and synchronize exactly once after battle.
6. Initiative/turn actor/action economy prevents out-of-turn and double actions.
7. Same idempotency key replays identical dice/outcome; changed payload conflicts.
8. Encounter completion grants XP/loot/BDE once.
9. BDE never becomes negative or double-spends under concurrent requests.
10. AI timeout/malformed/unsafe/stale proposal falls back or rejects without mechanic mutation/BDE charge.
11. Predetermined choice IDs are bound to the current legal-action-set digest.
12. Admin-only template mutation and campaign-owner instantiation boundaries hold.
13. Event eligibility, cooldown, safety, consent, and veto are deterministic.
14. Party member cannot read enemy hidden fields, another character's private fields, or another campaign.
15. Rehydrated play/encounter state after restart preserves mode, phase, active actor, revisions, effects, resources, and content pins.
