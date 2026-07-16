# Slice 5 ingestion/quarantine foundation evidence

Date: 2026-07-15

This evidence covers only the deterministic local Markdown ingestion and quarantine foundation. It does **not** establish a complete consent system or make real-life inspiration safe to enable for release.

## Boundary implemented

- `INSPIRATION_ENABLED=false` returns an empty review without inspecting the configured private source tree.
- An enabled load keeps the existing canonical-root, no-symlink, no-traversal, strict file/count/depth/byte-limit, and strict-UTF-8 boundaries. Root, filesystem, traversal, and limit failures abort the load.
- `load_dir_reviewed` quarantines malformed or conservatively flagged candidates and continues with structurally safe candidates. Duplicate approved IDs quarantine every ambiguous candidate.
- The source digest is SHA-256 over the exact source bytes.
- Raw Markdown is parsed only inside `events.rs` and discarded after review. Text under `## Fantasy transformation` is never retained as an instruction.
- The selector receives only approved prompts, so an enabled but quarantined source has no weight and cannot be selected.

## Data retained

An approved runtime prompt retains exactly:

- typed eligibility metadata (`schema_version`, opaque source ID, display title, bounded weight/levels/cooldown, sensitivity tags, pseudonymous participant aliases, enabled state);
- the exact-byte source digest;
- one to four whitespace-normalized, bounded, single-line plain-text facts extracted from `## Inspiration`;
- the compiled `HighFictionDistanceV1` transformation-policy identifier.

It retains no filesystem path, filename, raw Markdown body, or source-authored transformation instruction. Its `Debug` output redacts the title, tag values, aliases, and fact text.

A quarantine summary retains exactly:

- an opaque ID derived from the source digest rather than a filename or frontmatter field;
- the full exact-byte source digest;
- a sorted set of closed finding codes.

It retains no raw body, path, filename, parser message, copied source ID, title, contact detail, or guessed identifier. Its `Display`, `Debug`, and JSON representations contain only those safe fields.

## Deterministic pre-screen

The closed scans cover compiled lexical markers for:

- active resources, HTML, fenced code, and links;
- common prompt/tool-injection phrases;
- likely handles, email addresses, phone numbers, street addresses, and employer/workplace references;
- block/curly/paired quotations;
- Q11 prohibited sensitive-category vocabulary.

These are conservative heuristics, not a completeness claim. They can false-positive and can miss obfuscation, unfamiliar formats, multilingual text, names without contact syntax, novel prompt injection, and context-dependent or euphemistic sensitive material. They do not prove consent, ownership, age, audience/media permission, expiry, fictional distance, or provider privacy. Human review plus the independent consent/source registry remains required before the deployment gate may be enabled.

## Verification

```text
cargo test -p manchester-dnd-server events::tests --lib
15 passed; 0 failed

cargo test -p manchester-dnd-server context::tests --lib
3 passed; 0 failed

cargo clippy -p manchester-dnd-server --all-targets -- -D warnings
passed

git diff --check -- crates/game-server/src/events.rs \
  crates/game-server/src/context.rs crates/game-server/src/error.rs \
  docs/planning/06-consent-privacy-safety.md prompts/events/README.md \
  docs/evidence/slice-5-ingestion-foundation.md
passed
```

The strict package-wide Clippy gate completed without lint allowances.
