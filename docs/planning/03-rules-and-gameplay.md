# Rules and gameplay

## Rules profile and source boundary

The initial profile ID is `srd-5.1-cc`, meaning the 2014-era fifth-edition mechanics and reusable expression published in [SRD 5.1 under CC BY 4.0](https://www.dndbeyond.com/srd). The [2018 Basic Rules](https://media.wizards.com/2018/dnd/downloads/DnD_BasicRules_2018.pdf) may inform conformance review, but it is reference-only and is not a source for bundled prose or data.

SRD 5.2.1 represents revised rules and terminology published later. It is not an “upgrade” flag for this engine. Supporting it requires a separately named rules adapter, compendium, test suite, and explicit campaign creation/conversion flow.

## Target authority split

The current end-to-end surface implements the persisted `inspect-viaduct-runes`
check, the deterministic fixed Q04 encounter, authoritative created-hero encounter
stats, and the bounded level 1→2 hero workflow. A pure `rules_matrix` module also
implements and tests the broader level 1–2 mechanics listed below. Those broader
spell, class-resource, rest, condition, inventory, cover, and exploration/social
resolvers remain `implemented_not_exposed`: the application, persistence, AI, and UI
must not present them as playable until their capability is moved into a new pack and
the complete path has acceptance evidence.

| Deterministic Rust engine | AI GM |
| --- | --- |
| Validates commands and targets | Describes a scene and plays NPC voices |
| Determines legal actions and resource costs | Suggests relevant legal actions without limiting free-form input |
| Generates and records dice | Converts free-form intent into a typed proposal |
| Resolves checks, saves, attacks, damage, conditions, initiative, and advancement | Proposes a closed `CheckDifficulty`/`RewardTier` or known attack/target IDs, never raw DC/AC/modifier/XP values |
| Commits revisioned state and turn audits | Narrates facts already committed by the engine |
| Rejects unsupported or contradictory mechanics | Repairs malformed proposals or falls back to templates |

The AI cannot choose a die result, alter a modifier, set HP/XP/inventory, invent a supported rule, make a hidden state visible, or claim a different outcome than the engine. When the rules do not cover an intent, the engine returns `unsupported_mechanic`; the UI offers an authored alternative or a clearly labeled, versioned homebrew rule after MVP.

## Engine contract

The pure resolution boundary is conceptually:

```text
resolve(state, validated_command, rng_cursor, ruleset_version)
  -> resolution { events[], rolls[], next_rng_cursor, explanation[] }
```

No wall clock, database, network, model, OS randomness, or UI state is read inside resolution. Commands are intent (`Attack`, `AttemptCheck`, `Move`, `EndTurn`, `TakeRest`, `ChooseLevelFeature`); resolution facts include outcomes such as `AttackResolved`, `DamageApplied`, and `TurnEnded`. MVP writes the resulting authoritative state to revisioned documents and the facts to a turn audit. A later complete event stream may derive state from ordered facts only after equivalence tests prove coverage.

The authored exploration check uses `AttemptExplorationCheckCommand`; its strict
shared schema contains only schema version, campaign/character/action IDs, expected
revision, and idempotency key. The fixed encounter similarly accepts typed
`CommitEncounterCommand` intent without client-authored mechanics. In both paths,
`GameApplicationService` supplies trusted rules, dice, and time and atomically stores
the outcome/audit. Browser reload reconstructs committed state and never rerolls it.

Each explanation item identifies a rule key, inputs, modifiers, and result without reproducing long rules prose. This powers a player-facing “why?” panel and conformance tests.

## Dice and auditability

- Parse a bounded grammar such as `NdS`, optional keep/drop, and signed constants only when a supported mechanic requires it. Reject zero/oversized counts, sides, arithmetic overflow, and unbounded expressions.
- The server derives a protected campaign/encounter stream and uses the pinned
  `chacha20-v1` algorithm. Audits retain an opaque seed reference and cursor span;
  raw seed material remains server-only.
- A `RollRecord` contains roll ID, expression, individual dice, kept dice, modifier components, total, roll purpose, actor/target IDs, advantage state, ruleset version, and RNG cursor range.
- Clients submit “roll this check,” never dice values. Animation visualizes the committed record.
- Advantage and disadvantage follow the profile's cancellation/stacking behavior. Attack-roll natural 20/natural 1 handling is distinct from generic ability checks; do not generalize critical rules to all d20 tests.
- Displaying or rechecking a historic turn consumes no new randomness. A correction writes a new state revision/audit; it never edits a historic roll.

The authored check and fixed encounter now retain canonical roll records, including
purpose, dice, modifiers, comparison, outcome, algorithm ID, opaque seed reference,
and cursor span where applicable. The pure rules-matrix resolvers still need an
application/persistence bridge before their additional rolls or resource mutations
can be described as durable gameplay.

A future competitive multiplayer mode can add seed commitment/reveal. It is not an MVP security claim.

## MVP coverage matrix

This matrix is the MVP target. A row counts as delivered only when it is implemented,
tested, and surfaced in the UI; “Later” is not delegated to the AI. The fixed Q04
encounter and hero creation/advancement paths cover their advertised subset. Pure
rules-matrix coverage is implementation evidence only until each broader row has an
application, persistence, and UI path.

| Area | MVP | Later expansion |
| --- | --- | --- |
| Ability model | Six abilities, modifiers, proficiency bonus, skills, saving throws, passive values needed by content | Expertise edge cases and broader feature interactions |
| d20 resolution | Ability checks, saves, attacks, DC/AC, advantage/disadvantage, situational modifiers, roll audit | Contests or variants only where the chosen SRD profile defines them and product design needs them |
| Time and turns | Exploration turns as commands; combat rounds, initiative order, movement budget, action, bonus action, reaction, object interaction where supported | Simultaneous/group initiative variants and tactical grid timing |
| Core actions | Attack, Cast a supported spell, Dash, Disengage, Dodge, Help, Hide, Ready, Search, Use an Object; contextual availability | Grappling/shoving and complex readied-trigger edge cases if not in first encounter set |
| Combat | Melee/ranged attacks, range, cover subset, AC, hit/miss, critical damage, damage types, resistance/vulnerability/immunity needed by shipped creatures | Full cover/visibility/underwater/mounted/flying rules |
| Health | Max/current/temporary HP, damage, healing, unconscious/death-state flow used by supported play, stabilization, rest recovery | Exhaustive massive-damage and environmental edge cases |
| Conditions | Prone, restrained, grappled, incapacitated, unconscious, poisoned, and any condition required by shipped abilities | Complete SRD 5.1 condition interaction matrix |
| Resources | Hit dice/rests and class resources required by supported level-1/2 options | Full multiclass and high-level resource interactions |
| Equipment | Currency, equipped/carried items, weapons, armor, consumables needed by supported packs; capacity policy documented | Full encumbrance variants, crafting, economy simulation |
| Magic | A small, published list of SRD 5.1 spells needed by supported characters/encounters, with explicit effect implementations | Broad spell catalogue, concentration and targeting edge cases, high-level magic |
| Exploration/social | GM-selected or rules-derived DC proposal, checks/saves, clocks/objectives, NPC attitude represented as game-specific state | Full travel, hazards, downtime, factions, and negotiation systems |
| Creatures | Authored, licensed stat blocks for a small encounter set with deterministic actions | Encounter balancing, broad bestiary, procedural tactics |
| Advancement | One complete level 1→2 path for every MVP-supported character option; atomic validation of choices and derived stats | Levels 1–20, multiclassing, feats if licensed/supported |

The machine-readable table and fail-closed gate are described in
[mechanic traceability evidence](../evidence/mechanic-traceability-gate.md). An
encounter or character option cannot ship if it references an unimplemented or
untraced mechanic.

## Turn lifecycle

Outside combat:

1. present a committed scene and safe player view;
2. accept a structured choice or free-form intent;
3. translate free-form text to a proposal without changing state;
4. validate the proposal and request clarification when actor, target, or intent is ambiguous;
5. resolve required rolls/effects and atomically save the new revision plus turn audit;
6. narrate the committed facts and present the next state.

In combat, the engine additionally enforces initiative, current actor, movement/action budgets, valid targets/ranges, reactions, durations, and end-of-turn/round effects. A narration timeout cannot strand the state machine: deterministic text and the next legal actions remain available.

## DCs and rulings

Some play requires a GM judgment rather than a fixed rule. Model this as a constrained proposal:

```text
RequestAbilityCheck { character_id, ability, skill_id?, difficulty: CheckDifficulty, reason }
```

`CheckDifficulty` is a closed enum. Trusted application/rules policy maps it to an SRD-profile DC and records both tier and final DC. Campaign policy can require player confirmation for high-stakes checks. Strict unknown-field decoding rejects raw `difficulty_class`, modifiers, or other injected mechanics; the final DC is never chosen after seeing the roll.

## Effects and content

Prefer a closed, typed effect vocabulary (`DealDamage`, `Heal`, `ApplyCondition`, `MoveEntity`, `SpendResource`, `CreateClock`, and so on) over embedded scripts. Each content definition declares required engine capabilities; pack loading fails when a capability is missing. Escape hatches are implemented as reviewed Rust rules keyed by stable mechanic ID, not model-authored code.

## Leveling

MVP uses the implemented SRD 5.1 XP thresholds and validated `award_experience` path. An AI may propose only a `RewardTier`; trusted campaign policy decides whether that maps to an XP amount, progress, treasure, or nothing before `game-core` mutates a character and produces an audit summary. A future milestone mode is an explicit campaign-pinned progression policy, not an invisible model ruling. When XP eligibility is met, a `LevelUp` workflow:

1. freezes incompatible adventure commands but does not corrupt an active encounter;
2. computes choices from the pinned rules/content versions;
3. validates every selection and dependency;
4. commits level, features, resource maxima, and HP change in one transaction;
5. records choices so later derived-state code never needs to guess.

Level-down and automatic ruleset migration are out of scope. A content pack may change presentation and suggest builds, but not change advancement mechanics unless explicitly classified as versioned homebrew.

## Save/history correctness

MVP persists canonical revisioned campaign/character documents and immutable turn audits. Loading the same saved revision must produce byte-equivalent canonical mechanical state, and a displayed historic turn must use its stored rolls/outcome rather than rerolling or regenerating it. Full reconstruction from a domain-event stream is a later evolution and must not be claimed until coverage tests prove every mutation replayable. See [persistence](05-persistence.md) for version strategy and [AI generation](04-ai-generation.md) for proposal schemas.
