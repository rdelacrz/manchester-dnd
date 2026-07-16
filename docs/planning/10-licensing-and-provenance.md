# Licensing and content provenance

This is an engineering policy, not legal advice. Obtain legal review before public/commercial release or use of third-party brands, personal stories, likenesses, or unclear model outputs.

## Rules-content decision

The initial reusable rules-content source is the **Creative Commons edition of SRD 5.1**, downloaded from the [official SRD page](https://www.dndbeyond.com/srd), under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/legalcode). This project chooses the CC path for SRD-derived content and should not mix in OGL-only obligations accidentally.

The repository already reserves `content/srd-5.1/` for normalized records with the stable `ruleset_id: srd-5.1-cc` and keeps the SRD notice in `THIRD_PARTY_NOTICES.md`; no rules prose has been imported into that content directory yet. Preserve that separation and verify the notice against the approved SRD preamble at release.

The user-linked [2018 Basic Rules PDF](https://media.wizards.com/2018/dnd/downloads/DnD_BasicRules_2018.pdf) is a reference for understanding/conformance only. Do not copy its prose, tables, illustrations, layout, or data into the repository merely because it is publicly downloadable. A mechanic needed in the product must be implemented from the CC-licensed SRD 5.1 source, expressed originally where appropriate, and traced to that source.

SRD 5.2.1 is a later revised rules corpus. Although the [official SRD page](https://www.dndbeyond.com/srd) also publishes it under CC BY 4.0, it is a future, separately identified rules profile whose stable application ID has not been chosen. Do not combine its terminology, classes, spells, tables, or mechanics into an `srd-5.1-cc` pack or existing save without explicit provenance and conversion design.

## CC BY 4.0 obligations

[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) permits sharing and adaptation, including commercially, subject to conditions including appropriate credit, a license link, and indicating modifications; it does not imply endorsement and may not cover trademark, privacy, publicity, or other rights.

For every distributed build containing SRD 5.1 material:

- copy the exact attribution requested in the CC edition's SRD 5.1 legal preamble into a prominent `NOTICE`/credits location; do not paraphrase that mandatory notice;
- link the SRD source and CC BY 4.0 license and identify that the project's implementation/presentation modifies or adapts material;
- follow the preamble's instruction about how Wizards attribution should be stated rather than adding promotional-sounding attribution;
- keep the notice available in source distributions, packaged content, and an in-app legal/credits view;
- do not add technical or contractual restrictions to the SRD material that would prevent exercise of CC-licensed rights.

The project may have its own code license and original-content license; those choices do not erase the SRD notice. Keep code, SRD-derived data/prose, original setting content, third-party assets, private prompts, and generated artifacts distinguishable so each can carry its actual terms.

## Excluded content and branding

- Do not assume material from commercial rulebooks, adventures, digital tools, wikis, streams, fan sites, or search snippets is reusable because the underlying mechanic resembles SRD content.
- Do not ship non-SRD settings, characters, maps, prose, artwork, logos, trade dress, or other product identity without a separate documented license.
- Do not scrape rules/content from D&D Beyond pages. Import only an archived official SRD 5.1 CC source through the reviewed pipeline.
- Use an original product name, logo, UI, setting, and art direction. “Manchester Arcana” is the current product name, still subject to name/domain/trademark review before public release.
- Avoid using “Dungeons & Dragons” or its logos as the product/pack brand. Any factual compatibility statement must follow the official SRD guidance, trademark law, and legal review without implying sponsorship.

## Provenance manifest

Every distributable content file and media asset must resolve to a provenance entry:

```text
asset_id, path_or_digest, media_type
origin: original | srd-5.1-cc | third-party | generated | private-user
title/description, creator/rightsholder, source_url_or_private_source_id
license_id, license_url, required_notice, modification_note
retrieved/created_at, reviewer, review_status
ruleset/content_pack IDs and versions
generator provider/model/config fingerprint, prompt-template version, input hashes
consent/policy reference where personal material is involved
```

Do not put private source text, API keys, or confidential provider responses in this manifest. Public releases generate a redacted attribution/credits view from approved fields. Missing/ambiguous provenance quarantines an asset; “found online,” “AI-made,” or “fair use” alone is not an acceptable license record.

## Content classifications

| Class | May be bundled? | Required handling |
| --- | --- | --- |
| SRD 5.1 CC material | Yes | Exact SRD preamble attribution, CC link, modification indication, source trace |
| Original project prose/data/art | Yes | Named author/rightsholder and selected project/content license |
| Third-party open asset | Only after review | Verify exact asset/version, license compatibility, attribution and modification requirements |
| Proprietary/reference material | No by default | Reference locator only; no copied expression/assets without permission |
| Model-generated artifact | Only after provider/policy review | Provider/model/config provenance, input-rights review, safety review, no unsupported exclusivity claim |
| Private inspiration source | Not in public bundles | Consent, restricted storage, purpose/audience limits, deletion policy |

## AI provider and generated-content review

Before enabling a model/provider, record and periodically re-review:

- terms governing input rights, output use, retention/training, region/subprocessors, moderation, attribution, account tier, and prohibited uses;
- whether private/personal inputs are allowed under the chosen service settings and deployment policy;
- whether generated outputs can be redistributed commercially and what warranties/indemnities do or do not exist;
- a process for copyright/trademark/likeness complaints and takedown;
- model/config identity and generation date for each retained artifact.

Users and packs must have rights to inputs they provide. Generation does not cleanse an infringing input, and visual/text similarity can still create risk. Do not promise that generated output is unique, copyrightable, or exclusively owned. Run similarity and brand/identity review appropriate to release scope; private campaign visibility does not eliminate consent/privacy duties.

## Software dependencies and fonts/assets

- Record dependency licenses and notices from Cargo, npm/tooling, fonts, icons, CSS, and container/base images; source-code license compatibility is distinct from content licensing.
- Pin dependencies/assets by version/digest and generate an SBOM plus third-party notices for releases.
- Reject packages with unknown/disallowed licenses until reviewed; do not rely only on repository-level license labels when individual assets differ.
- Keep local development placeholders clearly marked and prevent unlicensed mock assets from entering release bundles.

## Contribution and pack intake

Before accepting public contributions or packs, choose a code/content contribution policy (for example, attestation that the contributor has rights) and document inbound/outbound licenses. Pack submission must enumerate every asset, its provenance, modifications, consent where applicable, and dependencies. Reviewers validate source URLs/files and license text rather than trusting manifest claims.

## Automated and manual release gate

CI should fail on missing provenance entries, unknown license identifiers, missing required notices, dependency policy violations, unpinned remote assets, or an SRD 5.2.1 source inside the SRD 5.1 corpus. A human release reviewer then:

1. compares the bundled SRD source digest with the approved official archive;
2. confirms exact SRD attribution and modification notice in package and UI;
3. samples rule/content traceability and all new third-party/generated assets;
4. reviews product name, marketing screenshots, compatibility language, and credits;
5. signs a license/provenance report retained with the release artifact.

The implemented mechanic gate is `python3 scripts/validate_mechanic_traceability.py`.
It joins the active core-pack declarations and
`content/packs/core-mvp/mechanics/traceability.json` to the versioned source registry
in `content/mechanics/engine-traceability.json`. The gate fails when:

- an active content entry lacks a source, implementation, real Rust test, consumer,
  or advertised capability;
- the manifest capability set differs from the capabilities reachable through active
  content;
- a source key is missing, duplicated, unused, carries an inconsistent license class,
  or resolves to a stale local digest;
- the approved SRD 5.1 document URL, version, SHA-256 digest, CC BY 4.0 legal-code
  link, modification statement, or required notice changes;
- a named Rust implementation/test symbol does not exist, or a public resolver/test
  in the bounded `rules_matrix` module is not represented; or
- an implementation-only capability claims a content consumer or appears in the
  pack manifest before an application/encounter integration makes it reachable.

The CI gate deliberately does not download the SRD. It checks the reviewed official
document pin and leaves the independent archive comparison to the human release step
above. It also does not replace legal review or prove that an implementation conforms
to every source rule; it proves that the declared, bounded mechanic surface has no
missing traceability link. See the
[mechanic traceability evidence](../evidence/mechanic-traceability-gate.md) for scope,
negative tests, and current integration gaps.

Licensing decisions that remain open are tracked in the [decision register](11-decision-register.md).
