# Slice 6 asynchronous scene-image evidence

Date: 2026-07-15

Status: implemented for the loopback `local-single-user` MVP with the
deterministic fake, disabled, and operator-configured OpenAI-compatible image
adapters. This evidence does not approve a real provider's contractual terms or
enable hosted mode.

## Typed policy boundary

`ImageBrief` is a versioned, closed Rust DTO reconstructed after enqueue or
restart from one committed `EncounterResolved` audit. Its five visible facts,
art direction, composition, exclusions, teen safety rating, fictionalization
policy, and authored alt-text context are all engine-owned enums or bounded
strings. There is no field for narration, player input, source Markdown,
private-inspiration facts, participant/source IDs, names, likeness references,
hidden state, paths, URLs, or provider instructions.

The service recomputes the brief and provider prompt from the source turn before
every attempt and compares the brief, prompt-policy, and non-secret provider
configuration fingerprints with the durable job. A mismatch fails closed. The
preflight policy requires original fictional figures, excludes recognizable real
people and exact landmarks, and permits only a non-graphic teen-fantasy scene.
The application records policy decisions as fingerprints and closed status
values, never the prompt or a private input body.

Images are manual only. A transaction reserves governance before enqueue and
enforces exactly three requests per rolling 24 hours, ten per campaign lifetime,
two per source turn (the initial image plus one replacement), one running image
job per campaign, and the configured campaign monetary ceiling. A paid-capable
profile cannot start without a non-zero operator cost estimate. In the local MVP
the sole owner and campaign are also the account boundary; hosted mode remains
unavailable rather than pretending this is cross-account authorization.

## Durable worker and failure behavior

PostgreSQL owns the job, attempt, governance, lease, retry, cancellation,
artifact, selection, quarantine, and expiry state. Enqueue is keyed by campaign,
purpose, client idempotency key, source turn, revision, and brief fingerprint.
An exact retry returns the existing job even after the response is lost.

The dedicated illustration worker claims only illustration jobs. It uses a
60-second lease, renews every 15 seconds, allows at most one running image per
campaign, applies the queue's bounded attempts and exponential backoff, and
treats cancellation, lease expiry, or lease takeover as authoritative. A fresh
service reconstructs a claimed brief solely from PostgreSQL. Provider calls have
the configured deadline and bounded response body; three retryable failures open
a 60-second circuit.

The real adapter sends only to one startup-approved base origin, follows no
redirects, rejects non-HTTPS remote endpoints, rejects non-loopback direct IP
endpoints, and never fetches an image URL returned by a provider. The worker
requires bounded base64 bytes. Fake and disabled adapters are deterministic and
network-free.

## Quarantine, publication, and retention

Provider bytes are untrusted until publication. The worker:

1. limits encoded and decoded byte counts;
2. identifies the actual signature and accepts only PNG or WebP;
3. decodes with 4,096-pixel dimension and 16,777,216-pixel allocation bounds;
4. rejects empty/all-transparent safety results;
5. re-encodes a metadata-free PNG original, a maximum-1,600-pixel web variant,
   and a maximum-512-pixel thumbnail; and
6. hashes every variant before writing it atomically under a protected root.

The protected root rejects symlinks and paths resolving through `public` or
`target`; Unix directories and files are mode `0700` and `0600`. It is not a
static-file root. SQL persists the source turn, creation/publication time,
provider/model, brief/policy/config fingerprints, protected relative keys,
dimensions, MIME, variant hashes, provider/application acceptance status,
selected/superseded state, estimated/actual cost, license identifier, provenance
summary, and bounded alt text.

Invalid bytes are body-free in SQL and optionally retained under a protected
quarantine key for 14 days. A replacement moves the previous valid version to a
30-day superseded retention window while the selected version lives with the
campaign. The scheduled cleanup removes protected files before deleting expired
metadata. The integration test expires both a superseded artifact and a
quarantine, deletes four files and two records, and leaves the selected artifact
available.

Only selected, campaign-owned, application-accepted `web` and `thumbnail`
variants can be delivered. The server has no original route. Each read
canonicalizes the protected key, checks campaign ownership, file kind/size, and
SHA-256 digest, and maps absence or wrong ownership to `404`.

## Playable browser acceptance

The Leptos panel includes an accessible manual request, non-image placeholder,
live queued/running/retry/rejected/unavailable/cancelled states, cancellation,
one-second polling, a verified result with meaningful alt text, replacement
control, remaining-request counters, next-request cost estimate, and hard-cap
status. The ordinary encounter controls remain usable throughout.

`tests/browser/slice6-scene-images.spec.ts` runs a release build against an
isolated real PostgreSQL database and the deterministic image adapter. It:

1. creates a rules-valid hero and commits an encounter;
2. snapshots the mechanical result;
3. drops the first enqueue response and proves an exact retry reuses the job;
4. waits through the placeholder for an authorized verified PNG with alt text;
5. proves the original route is `404` and the delivered variant is not publicly
   cacheable;
6. reloads and proves the selected artifact is durable;
7. requests the sole allowed replacement and observes the new selected image;
8. proves a third request is unavailable; and
9. proves the encounter mechanics did not change.

Result on 2026-07-15:

```text
cargo leptos build --release
passed

npm run test:browser:slice6
1 passed (5.6s)
```

## Automated control evidence

```text
scripts/validate-migrations.sh --static-only
migration validation: 21 ordered migration files passed static checks

cargo test --locked -p manchester-dnd-server config::tests -- --nocapture
11 passed; 0 failed

cargo test --locked -p manchester-dnd-server scene_images::tests -- --nocapture
7 passed; 0 failed

cargo test --locked -p manchester-dnd-server \
  repository::jobs::tests::illustration_ -- --nocapture
3 passed; 0 failed

cargo clippy --locked --workspace --all-targets -- -D warnings
passed
```

The focused suites cover exact Q09 constants, transactional daily/lifetime/turn
caps, one-campaign concurrency, one replacement, duplicate enqueue, restart
reconstruction, campaign authorization, selection/expiry cleanup, cancellation
races, provider URL rejection without fetch, signature spoofing, dimension/pixel
bounds, transparent safety rejection, metadata stripping, protected-path
traversal, and selected-versus-superseded delivery.

## Deliberate release constraints

- No real provider is approved by these tests. Before enabling one, the operator
  must record its output rights, retention/training, region, deletion,
  moderation, similarity, likeness, and takedown terms under Q08 and the release
  gate.
- Application validation proves technical integrity and the absence of a private
  input channel; semantic output moderation for a real deployment also depends
  on the approved provider's enforcement. Provider rejection is terminal and
  retained only as bounded metadata.
- Hosted accounts remain fail-closed. Cross-account object authorization must be
  implemented and tested before hosted image delivery can be enabled.
- Scene images are presentation-only. Character portraits, maps, uploads,
  reference photographs, automatic generation, and real-person likenesses are
  outside the MVP boundary.
