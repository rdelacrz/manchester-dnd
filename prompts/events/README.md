# Real-life-inspired event packs

Private event inspirations are Markdown files with a small JSON metadata object between `---` lines. Runtime loading defaults to `prompts/events/private`, whose contents are ignored by Git. Ingestion retains only one to four bounded plain-text facts from `## Inspiration`; it computes an exact-byte source digest and discards the raw Markdown.

```md
---
{
  "id": "late-night-bus-detour",
  "title": "The Carriage Takes the Long Road",
  "weight": 8,
  "minimum_level": 1,
  "cooldown_turns": 12,
  "sensitivity_tags": ["travel-mishap"],
  "participant_aliases": ["participant:11111111111111111111111111111111"],
  "enabled": true
}
---

## Inspiration

A routine trip unexpectedly became a long, funny detour.

## Fantasy transformation

Use a hired carriage, an enchanted route marker, and an inconvenient but non-lethal destination. Do not preserve locations, dates, names, or identifying details.
```

`## Fantasy transformation` is optional author/reviewer context. Runtime ingestion ignores its text and always applies the compiled `HighFictionDistanceV1` policy; source files cannot add model instructions or weaken that policy.

Before enabling a file:

1. Everyone represented must opt in and choose an alias.
2. Add all applicable sensitivity tags; a session must explicitly allow them.
3. Remove identifying detail that the scene does not need.
4. Choose a cooldown so one person or memory does not dominate play.
5. Delete or disable the file when consent is withdrawn.

The generation boundary receives only the minimized fact brief and compiled transformation policy when the deterministic selector marks an approved source eligible. It never decides which private source file to read. Deterministic lexical screening is a conservative pre-screen, not a substitute for the still-required human review and consent registry.
