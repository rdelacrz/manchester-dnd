# Rules and gameplay

## Rules profile and source boundary

The initial profile ID is `srd-5.1-cc`, meaning the 2014-era fifth-edition mechanics and reusable expression published in [SRD 5.1 under CC BY 4.0](https://www.dndbeyond.com/srd). The [2018 Basic Rules](https://media.wizards.com/2018/dnd/downloads/DnD_BasicRules_2018.pdf) may inform conformance review, but it is reference-only and is not a source for bundled prose or data.

SRD 5.2.1 represents revised rules and terminology published later. It is not an “upgrade” flag for this engine. Supporting it requires a separately named rules adapter, compendium, test suite, and explicit campaign creation/conversion flow.

## Target authority split

The current foundation implements ability modifiers, validated d20 checks/attacks, action-resource accounting, XP thresholds, numeric level derivation, and an atomic audited XP update. Slice 1A additionally exposes one persisted authored check: `inspect-viaduct-runes` is Wisdom, proficient, DC 13, with normal roll mode and no situational modifier. Saving throws, damage, conditions, initiative, playable combat, equipment, spells, HP mutation, and level-feature choices in the matrix below remain planned engine work; the AI and UI must not simulate them as if they were implemented.

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

The implemented concrete command is `AttemptExplorationCheckCommand`. Its strict shared schema contains only schema version, campaign/character/action IDs, expected revision, and idempotency key; unknown fields and attempts to submit mechanics are rejected. `GameApplicationService`, outside the pure rules crate, chooses the authored `AbilityCheck`, supplies dice and time, assigns `EventActor::System`, and persists the validated result as `AbilityCheckResolved`. Browser reload reconstructs the latest check from that audit and never rolls again.

Each explanation item identifies a rule key, inputs, modifiers, and result without reproducing long rules prose. This powers a player-facing “why?” panel and conformance tests.

## Dice and auditability

- Parse a bounded grammar such as `NdS`, optional keep/drop, and signed constants only when a supported mechanic requires it. Reject zero/oversized counts, sides, arithmetic overflow, and unbounded expressions.
- The target server initializes a campaign/encounter RNG from an operating-system CSPRNG. Resolution will use a pinned deterministic PRNG algorithm; algorithm ID, seed material or verifiable seed reference, and cursor remain pending persistence work.
- A `RollRecord` contains roll ID, expression, individual dice, kept dice, modifier components, total, roll purpose, actor/target IDs, advantage state, ruleset version, and RNG cursor range.
- Clients submit “roll this check,” never dice values. Animation visualizes the committed record.
- Advantage and disadvantage follow the profile's cancellation/stacking behavior. Attack-roll natural 20/natural 1 handling is distinct from generic ability checks; do not generalize critical rules to all d20 tests.
- Displaying or rechecking a historic turn consumes no new randomness. A correction writes a new state revision/audit; it never edits a historic roll.

Slice 1A currently records the selected d20, roll mode, ability/proficiency/situational modifiers, DC, total, and success in `AbilityCheckResolved`. It does not yet implement the general dice-expression grammar, pinned PRNG/cursor record, initiative, attack/damage sequence, or HP transitions described by the target above.

A future competitive multiplayer mode can add seed commitment/reveal. It is not an MVP security claim.

## MVP coverage matrix

This matrix is the MVP target. A row counts as delivered only when it is implemented, tested, and surfaced in the UI; “Later” is not delegated to the AI. At present, only the Slice 1A ability-check path is playable end to end—initiative, combat, damage, and HP mutation are still pending.

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

Before implementation, convert this matrix into a traceability table of mechanic ID → source location → implementation → tests. An encounter or character option cannot ship if it references an unimplemented mechanic.

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
