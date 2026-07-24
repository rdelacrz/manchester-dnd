# Dependency advisory exceptions

This register is part of the private-MVP release gate. Exceptions must identify
the exact advisory, dependency path, exposure, compensating controls, owner,
and a review deadline. A vulnerability or an advisory not listed here remains
a hard failure in `cargo deny check advisories` and `cargo audit`.

## Active exceptions

| Advisory | Dependency path | Release assessment | Compensating controls | Owner | Review by |
| --- | --- | --- | --- | --- | --- |
| `RUSTSEC-2024-0436` (`paste` unmaintained) | Leptos/Tachys macro implementation → `paste 1.0.15` | Unmaintained notice, not a reported vulnerability; no safe upstream upgrade exists. It is compile-time macro support and is not a network or runtime parser boundary. | Lockfile pinning, warnings-denied builds, full tests, release SBOM, and a fail-closed scan for any new advisory. Replace when Leptos removes it. | Private-MVP maintainer | 2026-09-01 |
| `RUSTSEC-2026-0173` (`proc-macro-error2` unmaintained) | Leptos/RSTML procedural macros → `proc-macro-error2 2.0.1` | Unmaintained notice, not a reported vulnerability; no safe upstream upgrade exists. It executes only during trusted-source compilation. | Lockfile pinning, isolated CI builds, no untrusted build inputs, release SBOM, and a fail-closed scan for any new advisory. Replace when Leptos removes it. | Private-MVP maintainer | 2026-09-01 |


## Review procedure

At each release and no later than the review date:

1. Re-run `cargo deny check advisories` and `cargo audit` against a freshly
   updated RustSec database and verify the runtime SBOM.
2. Check whether the pinned Leptos line has removed the dependency or whether a
   compatible maintained release exists.
3. Reassess whether the notice has gained a vulnerability or exploitability
   information. If so, remove the exception and block the release.
4. Record a new review date only with a fresh, written assessment; do not widen
   the crate/version or advisory scope.
