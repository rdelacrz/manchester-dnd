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
| `RUSTSEC-2023-0071` (`rsa` timing side channel) | Inactive `sqlx-mysql` optional dependency recorded in `Cargo.lock` and `tests/fuzz/Cargo.lock`; the release gate's `cargo tree --locked --all-features --target all -i rsa` checks have no path from either workspace | `cargo audit` scans every lockfile record and therefore reports this medium-severity advisory, but the application and fuzz harness enable only PostgreSQL (and feature-gated SQLite import) with SQLx defaults disabled. The vulnerable RSA crate is neither compiled nor shipped. | `cargo deny` checks both resolved target/feature graphs and must pass; CI repeats both no-path assertions before applying either `cargo audit` lockfile exception; container SBOM/scanning verifies that `rsa` is absent from the runtime artifact. | Private-MVP maintainer | 2026-09-01 |

## Review procedure

At each release and no later than the review date:

1. Re-run `cargo deny check advisories` and `cargo audit` against a freshly
   updated RustSec database. For the `rsa` exception, also prove that
   `cargo tree --locked --all-features --target all -i rsa` has no dependency
   path in either lockfile and that the runtime SBOM contains no `rsa`
   component.
2. Check whether the pinned Leptos line has removed the dependency or whether a
   compatible maintained release exists.
3. Reassess whether the notice has gained a vulnerability or exploitability
   information. If so, remove the exception and block the release.
4. Record a new review date only with a fresh, written assessment; do not widen
   the crate/version or advisory scope.
