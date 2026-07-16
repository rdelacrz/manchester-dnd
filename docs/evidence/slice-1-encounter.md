# Slice 1 deterministic encounter evidence

Status date: 2026-07-14. This evidence applies to the fixed private-MVP encounter
`manchester-arcana-content:v1:encounter:soot-wight-at-viaduct` under rules profile
`srd-5.1-cc`. It does not claim support for unlisted creatures, classes, spells, or
actions.

## Authoritative path

The browser submits only a strict `CommitEncounterCommand`: campaign and encounter
IDs, separate expected revisions, an idempotency key, and one typed intent. Actor,
dice, AC, modifiers, damage, HP, reward, timestamp, and RNG state are absent. The
application projects the stored event stream, derives the campaign seed through the
protected `SeedVault`, resolves the pure `game-core` state machine, creates canonical
roll records, and commits the successor session, immutable event, correlation ID,
and response receipt in one PostgreSQL transaction.

The retained roll contract is `chacha20-v1` with an opaque seed reference and exact
word cursor before/after. No response or audit contains raw seed material. Rendering,
reload, retry, receipt replay, history projection, and corruption verification consume
stored records; only a newly accepted command advances the stream.

## Rules and deterministic vectors

`crates/game-core/src/encounter.rs` covers:

- fixed Soot Wight and Canal Warden state, positions, AC, movement, temporary/current
  HP, initiative, round/current actor, turn resources, objectives, and transitions;
- stable initiative tie ordering, movement/range/target checks, action and object
  interaction spending, per-turn reset, attack/natural-1/natural-20/critical damage,
  death saves, stabilization, death, and the pinned story-recovery policy;
- the authored sluice action and successful/failed rune-check opening consequences;
- victory, defeat, reward eligibility, exploration transition, corrections, and
  deterministic authored narration/fallbacks.

`crates/game-core/src/roll.rs` supplies the bounded expression grammar, deterministic
stream, advantage/disadvantage cancellation, canonical `RollRecord`, golden stream
word vector, cursor replay, overflow rejection, and broad seed/cursor bound tests.

Local verification on 2026-07-14: `cargo test -p manchester-dnd-core` passed 107/107.
The suite includes exhaustive d20 attack outcome checks, HP/damage ranges, all death
save branches, non-overspend, actor/target forgery, state round trips, strict unknown
field rejection, and repeated command/roll replay equality.

## PostgreSQL and application evidence

Focused SQLx cases in `crates/game-server/src/application.rs` prove:

- encounter start commits canonical adjacent cursor spans and reloads through a new
  service instance without rerolling;
- an exact receipt retry returns the original response and appends no event;
- stale campaign and encounter revisions are distinct and spend no RNG cursor;
- concurrent duplicates converge on one committed result;
- a failed exploration result deterministically changes the opening state.

Repository tests prove the session/event/receipt transaction rolls back in full when
receipt insertion fails, and that stored rows reject identity, sequence, revision, and
schema inconsistencies. Event replay revalidates every stored roll against the
protected deterministic stream and rejects forged state or roll records.

## Browser evidence

`tests/browser/slice1-encounter.spec.ts` follows legal actions to a terminal encounter,
reloads and compares the saved mid-combat revision/HP/current actor, expands the
accessible roll explanation, and checks source, algorithm, cursor, and opaque seed
reference. During an active encounter it also:

1. replays a captured browser command with forged actor/roll fields and requires the
   redacted `400 invalid_server_input` boundary;
2. submits a schema-valid attack belonging to the other combatant and requires
   `invalid_encounter_command`;
3. reloads and proves neither rejected request changed the canonical scene.

The first local run exposed a real URL-form boundary defect: a movement distance was
encoded as text while a custom tagged enum required a numeric scalar. The command now
accepts only canonical unsigned decimal form scalars at URL-form boundaries, with
unit coverage rejecting signs, leading zeroes, fractions, overflow, and empty values.
The production browser matrix must be rerun after the final concurrent Slice 2 build;
until that run is green, this section is configured evidence rather than an acceptance
claim.

## Remaining acceptance work

- Record a green production Chromium, Firefox, and WebKit desktop/mobile run after
  the release build is regenerated.
- Keep success and defeat browser paths as distinct fixtures once campaign creation
  can allocate isolated campaign IDs; core golden vectors already cover both.
- Link the reviewed CI run and retained Playwright report before checking the final
  Slice 1 browser acceptance box.
- The Slice 2 created hero is not yet the fixed encounter participant. That integration
  is tracked in Slice 2 and must not be implied by this fixed-hero evidence.

## Reproduction

```sh
docker compose up -d --wait postgres
export DATABASE_URL=postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/manchester_arcana

cargo test -p manchester-dnd-core
cargo test -p manchester-dnd-server
cargo leptos build --release
PLAYWRIGHT_ADDRESS=127.0.0.1:6791 \
npx playwright test tests/browser/slice1-encounter.spec.ts
```
