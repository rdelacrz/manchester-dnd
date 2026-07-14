# Themed characters and content packs

## Design goal

Themes should change the fiction, presentation, suggested combinations, and authored hooks without quietly changing SRD mechanics. A rain-soaked industrial-fantasy hero and a classic high-fantasy hero may use the same stable `srd-5.1-cc` class/equipment mechanic IDs while receiving different names, descriptions, portraits, locations, and story prompts.

Mechanical homebrew is a separate, explicitly enabled pack class after MVP. It has stronger review, capability declarations, compatibility tests, and a visible campaign badge.

## Character creation workflow

MVP uses a resumable server-validated wizard:

1. **Campaign and theme:** select one compatible, licensed theme pack and safety profile.
2. **Concept:** choose an authored concept or ask AI for suggestions from only valid options.
3. **Rules-profile ancestry/race and class:** show the terminology and mechanics of `srd-5.1-cc`; filter to the options fully implemented by the engine.
4. **Ability scores:** support one MVP method chosen in the decision register; compute modifiers in Rust and show the audit/result for any random method.
5. **Background and proficiencies:** present valid choices, dependencies, and duplicate-proficiency handling.
6. **Equipment and resources:** select only supported starting options; validate capacity/equip rules used by MVP.
7. **Identity and presentation:** original name, pronouns, appearance, ideals/bonds/flaws or game-specific equivalents, tone limits, and optional generated portrait later.
8. **Review:** show derived AC, HP, saves, attacks, spell/resource summary, source/provenance labels, unsupported limitations, and advancement preview.
9. **Commit:** append one atomic `CharacterCreated` aggregate event plus explicit choice records; partial drafts remain non-authoritative.

AI suggestions return mechanic IDs plus short original rationale. Unknown/invalid IDs are discarded. The AI cannot generate ability totals, grant equipment/features, or skip required choices. A no-AI authored path is always available.

MVP ships at least two original presentation packs with the same supported mechanical coverage, proving that themes are data rather than conditionals in UI code. One may be Manchester-inspired, but must avoid implying endorsement or using real people/business brands without permission.

## Pack categories

| Category | Contains | Mechanical authority |
| --- | --- | --- |
| Rules compendium | SRD-derived mechanic definitions, tables, source keys | Vetted `game-core` `srd-5.1-cc` profile/content only; a separate adapter is a later option |
| Theme pack | terminology aliases, palette, original prose, name lists, art direction, character concepts | Presentation only |
| Adventure pack | original scenes, locations, NPC templates, objectives, encounter references | Can compose only supported mechanics |
| Creature/item/spell content | licensed definitions referencing typed effects and capability IDs | Vetted content; no scripts |
| Inspiration source set | private event-source manifests and eligibility tags | Separate consent pipeline; not redistributable by default |
| Homebrew rules pack | new/changed mechanics | Later, explicit opt-in and engine review required |

A single installable bundle can contain several categories, but each file declares its category and license/provenance.

## Manifest and layout

Each immutable pack version has a machine-readable manifest similar to:

```text
pack_schema: content-pack/v1
id: dev.manchester-d20.rainbound
version: 1.0.0
display_name: Rainbound Borough
categories: [theme, adventure]
compatible_rulesets: [srd-5.1-cc]
required_engine_capabilities: [check.basic, combat.attack, condition.prone]
dependencies: [{ id, version_requirement, digest? }]
license: { spdx_or_custom, notice_path }
provenance_manifest: provenance.json
content_roots: [...]
```

Content uses bounded JSON/YAML/Markdown schemas and references assets by digest. It cannot contain executable Rust/WASM/JavaScript, template evaluation, external fetches, or arbitrary HTML. File access is canonicalized beneath the pack root with size/count/decompression limits.

## Mechanical definitions

- Every rule-bearing entity has a stable namespaced mechanic ID, ruleset ID, schema version, typed effect list, and source/provenance key.
- Numeric effects and prerequisites use closed enums/fields; free prose never drives resolution.
- Capabilities are granular (`combat.reaction`, `spell.concentration`, for example). Loading fails if the engine lacks one required by reachable content.
- General rules and specific exceptions are encoded as ordered, named Rust policies with conflict tests. Pack load order cannot silently override a core mechanic.
- Derived character values are recomputed from recorded base choices/effects, not trusted from a pack or browser.

## Pack validation and activation

Validation is staged:

1. manifest/schema, digest, dependency, path, and size checks;
2. ruleset and engine-capability compatibility;
3. referential integrity and duplicate/cycle detection;
4. license/provenance completeness for every file/asset;
5. forbidden markup/instruction and safety linting;
6. mechanical fixtures that instantiate every reachable character/creature/action;
7. deterministic smoke encounter and hydration/render checks for presented content.

Installation quarantines invalid packs. Activation pins exact versions/digests to a new campaign; an existing campaign does not float to a newer version. Pack removal is blocked while an active campaign depends on it unless the exact version remains archived and readable.

## Theme application

Theme data can provide:

- design tokens, icon/placeholder assets, and art-direction vocabulary;
- original names for districts, factions, equipment presentation, and character archetypes;
- creation presets that point to valid mechanical choices;
- prompt fragments for tone and diction, treated as untrusted bounded data;
- event tags and fictional transforms compatible with the safety policy;
- accessible descriptions and non-colour cues.

Theme data cannot rename a mechanic in an audit trail so completely that the player cannot understand the underlying rule. The UI may show themed presentation first and a stable rules label/source key in details.

## Advancement and pack evolution

At character creation, store all choice IDs and pack pins. Level-up options come from the same compatible versions unless an explicit campaign pack migration succeeds. Removing or renaming a choice requires an alias/migration entry; changing mechanics requires a new version and conformance review.

Generated character biography and art are artifacts linked to the character and the pack/model provenance. They do not become a new content-pack dependency unless an author explicitly publishes them through the provenance/licensing workflow.

See [rules and leveling](03-rules-and-gameplay.md), [private inspiration handling](06-consent-privacy-safety.md), and [licensing](10-licensing-and-provenance.md).
