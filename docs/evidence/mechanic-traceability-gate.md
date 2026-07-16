# Mechanic traceability gate evidence

Status date: 2026-07-14. This evidence covers the machine-readable source and
capability gate for the current core MVP pack and the pure `rules_matrix` foundation.
It is not evidence that every pure rules-matrix resolver is reachable from the browser
or persisted encounter path.

## Declared scope

The active core pack has 35 rule-bearing entries and 25 advertised engine
capabilities. Each entry in
`content/packs/core-mvp/mechanics/traceability.json` now names actual Rust symbols and
actual `#[test]` function symbols rather than planning labels. The gate requires the
union of those active capabilities to equal the immutable manifest capability set.

`content/mechanics/engine-traceability.json` adds the source layer that the pack's v1
runtime schema intentionally does not carry:

- three versioned source documents with 46 explicitly bound source keys;
- the official SRD 5.1 PDF at version `5.1`, SHA-256
  `2504d2a0abb0a4d491a939be4f17910a2dde0312570ab8d208080225ccf0a1f0`,
  CC BY 4.0, its legal-code URL, the pack notice, and a modification statement;
- exact local digests for the accepted Q04/Q06/Q07 policy and rules-profile sources;
- 18 supplementary mechanic traces and 11 capability groups for the pure rules
  matrix; and
- an explicit `implemented_not_exposed` status with no consuming content for every
  supplementary capability.

The official source and license pins were verified against the
[D&D Beyond SRD page](https://www.dndbeyond.com/srd), the
[official SRD 5.1 PDF](https://media.dndbeyond.com/compendium-images/srd/5.1/SRD_CC_v5.1.pdf),
and the [CC BY 4.0 legal code](https://creativecommons.org/licenses/by/4.0/legalcode).
No Basic Rules, SRD 5.2, wiki, or third-party rules source is admitted by this gate.

## Executable checks

Run:

```sh
python3 scripts/validate_mechanic_traceability.py
python3 -m unittest scripts/tests/test_validate_mechanic_traceability.py
```

Local results on 2026-07-14:

- the positive repository gate passed;
- four validator tests passed;
- mutation tests proved failure for an advertised-capability mismatch, a nonexistent
  Rust test symbol, and a source key without a versioned document; and
- all 33 module-level public `rules_matrix` functions and all 23 focused tests were
  linked through supplementary mechanic traces.

The CI `migrations-and-docs` job runs both commands. Pack file, provenance, and
manifest digests are checked by the same script, so changing a trace row without
updating the immutable chain also fails.

## Honest reachability boundary

The 25 manifest capabilities are the active content-pack surface and are traced to
the existing hero/encounter implementations. The 11 `rules-matrix.*` capability IDs
are deliberately absent from that manifest. Their resolvers are implemented and
tested in pure `game-core`, but the application and persisted encounter do not yet
execute the expanded spell, spell-slot, class-resource/reaction, rest, condition,
inventory, cover, or exploration/social transitions as a unified runtime.

Moving any supplementary capability to active requires a new immutable pack version,
an application/encounter bridge, persistence and audit coverage, consuming content,
and UI/browser evidence. The gate will fail if someone merely adds a consumer or
advertises the capability without updating those declarations.

## Residual release work

- Independently archive/compare the approved official PDF at release; CI intentionally
  does not depend on the network.
- Complete human license, attribution, working-title, and trademark review before any
  public distribution.
- Render player-facing provenance/source labels and prove them in browser evidence.
- Reclassify only genuinely integrated rules-matrix capabilities in a new pack
  version; implementation-only status is not acceptance evidence.
