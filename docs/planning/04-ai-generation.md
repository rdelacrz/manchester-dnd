# AI text and image generation

## Purpose and boundary

Models make the campaign expressive; they do not make it mechanically true. Every generation call belongs to one of four explicit purposes:

1. **Intent parsing:** translate free-form player text into a typed, non-authoritative proposal.
2. **GM planning:** propose scene/NPC/check/encounter ingredients within engine capabilities and campaign safety policy.
3. **Narration:** render committed facts as concise prose without changing them.
4. **Illustration:** render a policy-approved visual brief asynchronously.

Each purpose has its own input DTO, output schema, prompt template, limits, provider policy, and fallback. Do not build one all-powerful “GM prompt” containing the entire database.

## Provider abstraction and runtime configuration

The implemented `game-server` traits isolate vendor APIs:

```text
TextGenerator.generate_text(TextGenerationRequest) -> TextGenerationResponse
ImageGenerator.generate_image(ImageGenerationRequest) -> ImageGenerationResponse
```

`OpenAiCompatibleGenerator` implements both traits; deterministic fake and disabled adapters keep CI/local development network-free and prevent accidental paid calls. The application now wraps illustration calls in durable PostgreSQL jobs with leases, retries, cancellation, governance receipts, artifact validation, and protected delivery; that orchestration remains deliberately outside the provider trait. Later adapters should declare structured-output, image-size, seed, moderation, and usage capabilities.

Startup configuration comes from the independent `TEXT_LLM_*` and `IMAGE_LLM_*` profiles loaded through `dotenvy`: backend, base URL, API key, model, timeout, text temperature/output tokens, and image size. No provider/model is hard-coded into `game-core`. Credentials never enter fingerprints, logs, client DTOs, or saved prompts.

MVP reads configuration at startup, so changing supported profile variables and restarting changes the active model without recompilation. Later, audited routing policies can choose among pre-approved configurations per purpose; an in-flight call always carries an immutable configuration snapshot.

## Implemented GM proposal contract

`game-core` already defines a strict versioned `AiGmProposal` that deserializes without applying itself:

```text
AiGmProposal {
  schema_version,
  proposal_id,
  session_id,
  based_on_event_sequence,
  narrative: { text, image_prompt?, choices[] }?,
  effects[]
}
```

The implemented tagged effects deliberately avoid raw mechanical authority:

- `RequestAbilityCheck { character_id, ability, skill_id?, difficulty: CheckDifficulty, reason }`;
- `RequestAttack { character_id, target_id, attack_id, reason }`, where both target and attack must come from known legal IDs;
- `ProposeReward { character_id, tier: RewardTier, reason }`, never an XP amount or item definition;
- `IntroduceEvent` with a source-prompt ID that must match any injected private inspiration;
- `EndSession`.

`CheckDifficulty` is a closed narrative tier that trusted rules policy maps to a DC; `RewardTier` is a closed signal that campaign policy may map to XP, progress, treasure, or nothing. The model never supplies raw DC, AC, attack modifiers, damage, XP, or arbitrary rules payloads. `#[serde(deny_unknown_fields)]` on the proposal, narrative, and tagged effects rejects attempts to smuggle those fields into JSON.

`GameMasterService` sends authoritative session/character/recent-event context plus player intent and optional already-filtered inspiration. It assigns every generation attempt a unique server ID, replaces provider-chosen IDs, validates the exact base sequence and legal character/skill/attack/target/source identifiers, and computes a canonical SHA-256 fingerprint over the validated proposal. Acceptance events retain the unique ID and fingerprint so a retry cannot substitute different content. Constructing/deserializing a proposal never changes state.

MVP must still strengthen the application layer before accepting an effect: authenticate/authorize the caller; map difficulty/reward tiers through campaign rules; confirm engine capability and the locked revision; enforce hidden information/safety; and convert each accepted effect through `game-core`. Excessive text and unsupported mechanics fail closed. At most a bounded repair attempt is allowed; then use a fallback or focused clarification.

As more purposes arrive, introduce separate versioned `ActionProposal`, constrained `CheckProposal`, and `SceneProposal` DTOs rather than overloading `AiGmProposal` compatibly in place. Illustration already uses its own closed, versioned `ImageBrief` reconstructed from a committed encounter audit.

Raw model JSON is diagnostic data, not authoritative state or a turn audit. A validated core/application result or saved presentation artifact is the durable result.

## Prompt assembly

Assemble requests from typed sections in a fixed order:

1. non-overridable purpose, output schema, rules-authority, privacy, and safety policy;
2. compact rules capability list and relevant mechanic IDs, not a whole rules corpus;
3. campaign style/content-pack guidance;
4. minimal safe view: current scene, permitted entities, recent committed facts, legal-action hints;
5. optional pre-filtered inspiration facts with opaque source IDs;
6. delimited player input, explicitly labelled untrusted data.

Markdown event files, player text, imported pack prose, and previous output are data, never instructions. Normalize and length-limit them; strip active markup; do not follow embedded tool, system-prompt, secret, or policy-change requests. Provider adapters have no shell, database, filesystem, or arbitrary network tools.

Store prompt template ID/version, policy version, input hashes, safe-view revision, and model configuration fingerprint. Avoid retaining full prompts by default, especially when they contain personal material. An opt-in encrypted debug capture has a short TTL and restricted access.

## Text turn flow

### Free-form player intent

1. Fast deterministic parsing handles UI-generated commands and obvious dice syntax.
2. A text model parses only genuinely free-form intent into `ActionProposal`.
3. Application validation checks campaign revision, actor authority, entity visibility, engine capability, and payload bounds.
4. Ambiguity produces clarification; illegality produces an explanation and legal alternatives.
5. The deterministic engine resolves the accepted command, saves revisioned state, and appends its turn audit.

### Narration

Narration receives an immutable fact bundle: event IDs, roll records, visible state delta, tone, length, and prohibited claims. Validate that referenced events exist, sanitize output Markdown to a small allowlist, and render it as untrusted text. A lightweight contradiction check ensures required facts/numbers are present and forbids conflicting outcomes; repeated failure uses deterministic templates assembled from the facts.

The campaign advances after mechanical commit even when narration is pending. Save only the selected narration version; alternates may be discarded or retained under an explicit user-visible regeneration history.

### GM planning

The model can propose a new scene, NPC, clock, reward category, or check. The application maps proposals through content/rules allowlists and campaign budgets. Mechanical rewards, encounter statistics, DC values, and inventory definitions are resolved from approved content or engine policy. Novel prose can be generated; novel mechanics cannot silently appear.

## Image flow

Images are optional presentation artifacts, never input to rules resolution.

1. After a committed encounter, the player may make a manual image request. Automatic generation is not an MVP capability.
2. The server reconstructs a closed `ImageBrief` from engine-authored visible fictional facts in the committed audit. It excludes player/narration text, private source material, names, contact data, hidden state, and real-person likeness descriptions.
3. Policy and governance validation run transactionally before enqueue. The worker recomputes the brief and fingerprints before submission; provider rejection and application validation failures fail closed.
4. A durable PostgreSQL job is leased by the illustration worker. Provider bytes enter quarantine, are signature/format/dimension/pixel checked, re-encoded to metadata-free PNG originals and bounded variants, hashed, and stored beneath a non-public protected root with provenance.
5. Only campaign-authorized selected web/thumbnail variants are delivered. Provider URLs and originals have no delivery route.
6. The UI shows a stable placeholder and queued/running/retry/rejected/unavailable/cancelled status, then swaps in the verified result with authored alt text. Failure never blocks the turn or save.

MVP supports one scene-image purpose and one configured provider at a time. Later support may include character portraits, maps, consistent-reference workflows, and provider routing, after rights, likeness, and safety review.

## Failure, retries, and idempotency

The adapter and durable orchestration supply:

- Every illustration call has a deadline, cancellation path, idempotency key, lease heartbeat/expiry, bounded exponential retry, and maximum attempt count.
- Retry transport failures and explicit provider throttling; do not blindly retry policy rejection or a consistently invalid schema.
- Circuit-break a failing provider and expose a non-alarming degraded-mode indicator.
- Narration fallback is fact-based templates; intent fallback is structured UI/clarification; image fallback is the pack's licensed placeholder.
- A duplicate completion cannot append a second turn/correction audit or overwrite an approved artifact unexpectedly.
- Users can request one image replacement, but an exact retry or replacement cannot reroll or modify the resolved turn.

The scene-image worker additionally rejects provider-returned URLs rather than fetching them, follows no HTTP redirects, uses only the startup-approved provider origin, applies per-campaign concurrency and circuit limits, and stores no full prompt body.

## Cost and latency controls

- Image governance enforces three requests per rolling 24 hours, ten per campaign lifetime, one initial plus one replacement per committed scene, one running job per campaign, bounded pixels/bytes, and the configured monetary hard cap. The loopback MVP's sole owner/campaign is also its account boundary; hosted accounts remain disabled.
- Compact recent context plus stored summaries; summaries are presentation context and cannot replace authoritative documents/turn audits.
- Cache only requests safe to share under the same campaign/revision/configuration; default cache scope is one campaign.
- Track queue time, provider latency, validation/repair rate, tokens/images, estimated cost, fallback rate, and user regeneration rate without high-cardinality private text labels.

## Model evaluation

Maintain a versioned, synthetic evaluation set covering legal/illegal actions, ambiguous intent, hidden-information attacks, prompt injection, rule contradictions, safety boundaries, and narration fidelity. Promotion of a new model/configuration requires threshold scores for schema validity and fact consistency, zero critical privacy leaks in the suite, acceptable cost/latency, and a recorded human review of representative creative output.

Related controls: [rules authority](03-rules-and-gameplay.md), [personal inspiration safety](06-consent-privacy-safety.md), and [generated-asset provenance](10-licensing-and-provenance.md).
