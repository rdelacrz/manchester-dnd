# Rewrite Part 1 — Orchestration

Source of truth: `.context/REWRITE_PART_1.md`
Board: `neomatrix`

## Mission

Implement every unchecked step in Tasks 1–23 in dependency order. Preserve the current uncommitted rewrite tranche; do not reset, stash, discard, or overwrite unrelated work.

## Execution order

1. Tasks 1–6: baseline, layouts, account/session/auth/CSRF.
2. Tasks 7–8: public pages, protected routing/navigation.
3. Tasks 9–11: level-less character library, campaign instances, UI.
4. Tasks 12–17: campaign membership, lobby, authorization.
5. Tasks 18–20: action points, AI substitution, play page.
6. Tasks 21–23: browser migration, security gate, compatibility/operations.

## Gates

- **Pre-flight:** prerequisite migrations/services/tests exist before dependent UI work.
- **Revision:** every implementation card receives specification review, then code-quality/security review; revise until approved.
- **Escalation:** ambiguous product policy or external production/TLS requirement blocks the relevant card with exact decision/evidence needed.
- **Abort:** never enable hosted mode while Task 22 evidence is incomplete.

## Rules

- TDD: observe a failing test before implementation where new behavior is required.
- Strict errors only; no broad catch-all suppression.
- PostgreSQL tests use an isolated test-capable development role.
- Do not commit failing or partial slices.
- Update `.context/REWRITE_PART_1.md` only after concrete evidence exists.
- Keep live test results in `TEST_RESULTS.md`; open review issues in `REVIEW_NOTES.md`.
- Archive resolved review notes under `.hermes-orchestration/_archive/`.
