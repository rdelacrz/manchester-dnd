# Real-life-inspired event packs

Private event inspirations are Markdown files with a small JSON metadata object between `---` lines. The remaining Markdown is guidance for fictionalization, not text to reproduce. Runtime loading defaults to `prompts/events/private`, whose contents are ignored by Git.

```md
---
{
  "id": "late-night-bus-detour",
  "title": "The Carriage Takes the Long Road",
  "weight": 8,
  "minimum_level": 1,
  "cooldown_turns": 12,
  "sensitivity_tags": ["travel-mishap"],
  "participant_aliases": ["the_cartographer"],
  "enabled": true
}
---

## Inspiration

A routine trip unexpectedly became a long, funny detour.

## Fantasy transformation

Use a hired carriage, an enchanted route marker, and an inconvenient but non-lethal destination. Do not preserve locations, dates, names, or identifying details.
```

Before enabling a file:

1. Everyone represented must opt in and choose an alias.
2. Add all applicable sensitivity tags; a session must explicitly allow them.
3. Remove identifying detail that the scene does not need.
4. Choose a cooldown so one person or memory does not dominate play.
5. Delete or disable the file when consent is withdrawn.

The AI receives a fictionalized event brief only when the deterministic selector marks it eligible. It never decides which private source file to read.
