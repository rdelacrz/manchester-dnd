# Consent, privacy, and safety

## Core rule

A real-life memory is sensitive source material, not ordinary flavour text. It may inspire a fictional event only when the source owner, every identifiable participant, and the current campaign policy permit the intended use. “Random” controls timing inside those boundaries; it never bypasses consent.

MVP supports administrator-installed Markdown from the private `EVENT_PROMPT_DIR` (default `prompts/events/private`, ignored by Git). It does not accept arbitrary public uploads. Source Markdown is never served to browsers, copied into exports, embedded in images, or sent wholesale to a model.

The deployment-wide `INSPIRATION_ENABLED` gate defaults to `false`. When it is
false, startup does not read the private source tree at all. A persisted
campaign opt-in is a second, narrower gate and can never override a disabled
deployment. A durable global incident switch independently blocks reservations,
cancels pending work, and quarantines completed private presentations across
process restarts.

## Consent model

Consent is specific, reversible, and recorded independently from the Markdown:

- who may use the source and in which campaigns;
- which identifiable people appear and whether each has opted in;
- allowed transformations and audience: private solo, named friend group, or shareable recap;
- allowed media: text inspiration, image brief, recap; real-person likeness is prohibited in MVP even if text use is allowed;
- sensitivity/lines, fictional distance, tone, expiry, and review requirement;
- whether past generated artifacts may remain after revocation.

The campaign begins with a safety setup covering tone, age rating, lines, veils, phobias/topics, real-life inspiration on/off, and participant-specific exclusions. Defaults are conservative: inspiration off, no minors, no health/trauma/sexual/criminal/financial/relationship secrets, no protected-trait jokes, and no current crisis material.

A participant can pause, veil, veto, or disable inspiration without explaining why. Revocation takes effect for eligibility immediately. It does not rewrite mechanical history; it can hide/delete derived presentation artifacts and replace them with a neutral redaction marker while retaining the minimum security audit required by policy.

## Implemented Markdown v1 contract

The current loader uses a JSON metadata object between `---` lines followed by a Markdown guidance body. IDs and `participant_aliases` are pseudonymous and stable; do not put real names, contact details, addresses, employer details, or account handles in either section.

```md
---
{
  "schema_version": 1,
  "id": "missed_last_carriage",
  "title": "The Carriage Takes the Long Road",
  "weight": 2,
  "minimum_level": 1,
  "maximum_level": null,
  "cooldown_turns": 20,
  "sensitivity_tags": ["travel-mishap"],
  "participant_aliases": [
    "participant:11111111111111111111111111111111",
    "participant:22222222222222222222222222222222"
  ],
  "enabled": false
}
---

## Inspiration

Two friends missed late transport and improvised a harmless route home.

## Fantasy transformation

Use a harmless carriage detour. Do not preserve names, quotations, dates, or locations.
```

The `## Fantasy transformation` section is author-facing review context only and is discarded during ingestion. It never becomes a model instruction. The runtime always uses the compiled `high_fiction_distance_v1` transformation policy.

The implemented loader denies unknown JSON fields, validates bounded schema/ID/weight/level/label metadata, quarantines duplicate IDs, defaults files to disabled, and requires enabled files to declare tags, participant aliases, and a positive cooldown. Participant aliases must use `participant:` plus a 32-character lowercase hexadecimal opaque identifier; descriptive names and handles are not accepted. It canonicalizes the configured root, bounds depth/count/file size, rejects traversal, symlinks and special filesystem entries, requires strict UTF-8, and redacts source paths from both normal and debug errors. When the deployment gate is off, startup does not inspect the source tree. Selection additionally requires the campaign-level inspiration opt-in plus level, sensitivity, participant-consent, and cooldown eligibility. `participant_aliases` are eligibility keys, not proof of consent; the application must construct the consenting-alias allowlist from independent current consent.

The loader extracts only one to four bounded, single-line plain-text facts under `## Inspiration`, normalizes whitespace, computes a digest of the exact source bytes, and then discards the raw Markdown. An approved runtime record retains typed eligibility metadata, that digest, the minimized fact brief, and the compiled transformation-policy identifier. A quarantined record retains only a digest-derived opaque ID, the full digest, and a sorted set of closed finding codes. It retains no path, filename, parser message, copied identifier, or source body. Audience/media permission, expiry, and blocked elements remain campaign/consent policy in v1; adding them to files requires `schema_version: 2` and compatibility tests rather than silently accepting new fields.

## MVP ingestion hardening

The loader now performs deterministic pre-screening for active resources/HTML/code fences/links, common prompt/tool-injection phrases, likely contact handles/email/phone/address/employer markers, direct quotations, and the prohibited sensitive-category vocabulary in Q11. Flagged or malformed candidates are quarantined while structurally safe candidates continue through review. This is deliberately conservative lexical screening, not a claim of complete PII, consent, context, age, or safety detection. It can produce false positives and can miss obfuscated, novel, multilingual, or context-dependent material.

Before enabling a reviewed private source, use the body-free operator pipeline:

1. Verify source-owner and human-review decisions in the independent durable source registry.
2. Verify pseudonymous participants and exact campaign/media/expiry consent before activation.
3. Register source digest/version, provenance, themes, media, participants, sensitivities, and review without copying the raw body into PostgreSQL.
4. Re-run registration, human review, and consent whenever a changed file produces a new exact-byte digest.
5. Exercise revocation, deletion, global quarantine, and backup-expiry paths using the [private-inspiration runbook](../operations/private-inspiration-runbook.md).

Changing a file creates a new source digest/version and requires revalidation. Consent is not inferred from Git access or from a contributor saying that something is funny.

## Random event selection

The `game-server` selector—not the model—controls whether and which inspiration source may be selected. The PostgreSQL transaction derives the trusted safe trigger, party level, campaign pins, safety exclusions, verified participants, exact grants, vetoes, cooldown, and deterministic random authority. It persists the canonical eligible-set digest, selected opaque source/version digest, rational draw, algorithm, cursor interval, cooldown, and no-selection reason before source-derived presentation is used. An ineligible set consumes no cursor, and exact request replay returns the same receipt.

1. An authored trigger window opens only at appropriate narrative boundaries, never mid-safety flow or during an incompatible combat state.
2. Filter by campaign opt-in, all participant consents, audience/media, safety settings, expiry, active source version, cooldown, theme compatibility, recent use, and campaign-specific vetoes.
3. Apply bounded configured weights; a source with no eligibility has zero chance regardless of weight.
4. Record an auditable server roll and selected opaque source ID/version.
5. Pass only the minimized facts and transformation policy to the text-generation boundary.
6. Validate the proposal again. If it is unsafe, overly similar, or reveals an identifier, discard it and use an unrelated fictional fallback; do not keep rerolling until private material passes.

The player-facing log may say “an opted-in memory inspired this event” with a source nickname only if policy permits. It never exposes the source body or consent record.

## Fictionalization rules

- Preserve a broad emotional/comedic motif, not exact sequence, wording, dates, locations, appearance, or identifying combinations.
- Replace people with fictional roles/species and remove one-to-one mapping by default.
- Never infer sensitive traits or embellish a memory into guilt, romance, abuse, illness, crime, addiction, or humiliation.
- Do not present the fiction as a factual account or use a participant's name in an image prompt.
- Prevent direct quotations and run similarity/identifier checks against the minimized source before release.
- Apply the campaign's configured tone and fictional-distance policy; high distance is the MVP default and minimum for group play.

## In-session controls

Always-visible controls include:

- pause generation and continue with deterministic presentation;
- veil/hide the current passage or image immediately;
- veto and regenerate without using the source;
- disable one source/category or all real-life inspiration for the campaign;
- report a privacy/safety issue and attach only opaque event/artifact IDs;
- review the private-campaign audience boundary. Public share links and direct canonical-document sharing do not exist in MVP.

An X-card-style veto is honored first and investigated later. The UI must not ask the player to justify it. A safety intervention is not shown as a failure attributed to a participant.

## Model and prompt-injection controls

Treat source files as hostile input even when locally authored. Delimit facts as data, use a closed schema, strip instruction-like sections, and state that source text cannot modify system policy or invoke tools. The model receives no consent database, filesystem path, contact data, secrets, other sources, or unrelated campaign history. Provider retention/training settings must satisfy the deployment's privacy policy before a provider is enabled.

Generated text is escaped/sanitized before rendering. Generated images pass provider and application policy checks and are never trained/referenced on a real participant's photos in MVP.

## Data rights and operations

- Provide a source inventory to authorized participants without revealing somebody else's private body text.
- Support access, correction, revocation, export, and deletion requests with documented response/backup-retention behavior.
- Encrypt sensitive source content and keep decryption access separate from ordinary game workers where practical.
- Audit source install/update, consent changes, eligibility selection, artifact creation, viewing of restricted diagnostics, revocation, and deletion using opaque IDs.
- Never place real-life text in metrics, tracing spans, crash reports, analytics labels, support tickets, or model-evaluation corpora.
- Incident response includes disabling generation globally, confirming that no MVP share route exists, quarantining artifacts, rotating credentials, notifying affected users as policy/law requires, and preserving a minimal investigation audit.

The offline `inspiration-admin` binary supplies the current source inventory,
verification, registration/review, safety setup, grant/revocation, scoped export,
participant deletion, global switch, and tombstone-expiry workflows. It accepts a
closed body-free JSON schema only. Participant deletion refuses to run while a
configured source still contains that pseudonym, then atomically revokes grants,
quarantines registry sources, cancels pending work, applies artifact policy, and
retains an opaque 35-day tombstone. See the [operator and incident
runbook](../operations/private-inspiration-runbook.md).

## Release gate

Real-life inspiration remains feature-flagged off until threat modeling and user testing demonstrate: opt-in defaults, multi-party consent enforcement, immediate veto/revocation, no raw source in client/network/log/export paths, deterministic eligibility replay, provider data-policy review, and a documented deletion exercise.

This design is a product safety baseline, not legal advice. Deployment owners must assess applicable privacy, biometric/likeness, child-safety, and data-protection law for their users and region.
