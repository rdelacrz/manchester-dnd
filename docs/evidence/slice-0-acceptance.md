# Slice 0 acceptance evidence map

Status date: 2026-07-14. This page tells reviewers what is executable now, where
results are emitted, and which Slice 0 claims remain unverified. A configured CI
job is not evidence until it has passed for the reviewed commit.

## Automated gates

| Gate | Command or CI job | Evidence produced | Status |
| --- | --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | CI log | Configured, not yet run in repository CI |
| Clippy warnings denied | `cargo clippy --locked --workspace --all-targets -- -D warnings` | CI log | Configured, not yet run in repository CI |
| Workspace unit/integration tests | `cargo test --locked --workspace` with PostgreSQL 17 | Test log and isolated SQLx databases | Configured, not yet run in repository CI |
| Production SSR and WASM | `cargo leptos build --release --bin-cargo-args=--locked --lib-cargo-args=--locked` | Release binary and `target/site/pkg` | Passed locally 2026-07-14; new CI gate not yet run |
| Migration order and PostgreSQL application | `scripts/validate-migrations.sh` | Static result plus isolated-schema application result | Passed locally 2026-07-14; not yet run in repository CI |
| Documentation links | `scripts/check-doc-links.sh` | Missing-path/heading diagnostics or pass count | Passed locally 2026-07-14; not yet run in repository CI |
| Checked-in credential signatures | `scripts/scan-secrets.sh` | Paths only; never credential bodies | Configured, not yet run in repository CI |
| Injected provider canaries | Dynamic canaries plus `scripts/scan-secrets.sh target/release/manchester-dnd-web target/site EVIDENCE_DIR` | Client binary/WASM/JS/CSS, SSR headers/body, safe errors, and server log scan | Two synthetic canaries passed locally 2026-07-14; not yet run in repository CI |
| Provider-disabled deployment smoke | `scripts/provider-disabled-smoke.sh` | Liveness/readiness/SSR/server-function headers and bodies; startup logs | Passed locally 2026-07-14; not yet run in repository CI |
| Browser/hydration/accessibility matrix | `npm ci && npx playwright install --with-deps chromium firefox webkit && npm run test:browser` | `target/playwright/report`, plus traces/screenshots/video on failure | Chromium desktop/Android and Firefox desktop/mobile passed locally (16/16); WebKit awaits CI system dependencies |
| Minimal runtime image | `docker build --pull -t manchester-arcana:COMMIT .` | Successful pinned multi-stage image build | Configured, not yet run in repository CI |

CI actions are referenced by immutable commit SHA. The build toolchain is Rust 1.90.0,
`cargo-leptos` is 0.3.7, Node is 22.17.0, npm dependencies have an integrity-locked
lockfile, the PostgreSQL service and Docker bases use immutable image digests, and
Playwright owns the exact browser revisions.

## Boundary behavior exercised by the smoke

| Scenario | Required observation |
| --- | --- |
| Process alive | `GET /health/live` returns `204`. |
| Database ready | `GET /health/ready` returns `204`. |
| Production SSR | `GET /` returns `200`, English shell content, CSP `frame-ancestors`, and `X-Frame-Options: DENY`. |
| Providers disabled | A valid same-origin server-function request loads the persisted campaign without a model call. |
| Malformed typed input | The transport returns `400` with only `invalid_server_input`; decoder details and submitted fields are absent. |
| Forged Origin | Gameplay server function returns the safe `invalid_request_origin` code. |
| Forged Host plus matching forged Origin | HTTP boundary returns `421` with the safe `invalid_request_host` code. |
| Hosted mode | A second process exits nonzero before binding and names `APP_ACCESS_MODE`, without values. |
| Invalid provider config | A second process exits nonzero before binding and names `TEXT_LLM_BACKEND`, without values. |
| Database unavailable after startup | With connections disabled and existing sessions terminated, liveness remains `204`, readiness becomes `503`, and readiness recovers to `204` after connections are restored. |

An Origin mismatch is a typed server-function rejection inside HTTP `200`; its
stable public error code, not transport status, is the contract. A Host mismatch is
rejected earlier by the general HTTP boundary with `421`. See the
[operator runbook](../operations/slice-0-runbook.md) for the exact local trust
boundary.

## Secret-canary procedure

CI creates two unpredictable, synthetic provider credentials at runtime, supplies
them to disabled provider profiles, retains the smoke response/log directory, and
scans all of these without printing the canary values:

- checked-in and newly added non-ignored files;
- release server binary;
- WASM, JavaScript, CSS, and other site artifacts;
- SSR body and response headers;
- valid and rejected server-function response bodies/headers;
- normal server log, hosted-mode startup error, and invalid-config startup error.

The scan also applies narrow private-key, AWS access-key, GitHub token, and OpenAI
token signatures. It is a high-signal guard, not a replacement for provider-side
secret detection, history scanning, rotation, or human review.

## Unverified or incomplete Slice 0 acceptance work

- The real operating-system/device and assistive-technology Q02 matrix is manual
  and has not been run. Linux Playwright emulation must not be presented as that
  evidence.
- The controlled PostgreSQL outage rehearsal is configured in CI. Its result is
  evidence only after the reviewed commit has a green run; cleanup always restores
  database connections before browser tests begin.
- Readiness does not check backup age, disk, event packs, providers, or budget.
- The native theme form and its hydrated enhancement now share the same bounded
  values; the suite submits it with JavaScript disabled and changes it in place
  after hydration. Encounter mutation still correctly requires the authoritative
  server function.
- The shell deliberately renders no server-generated IDs, timestamps, random
  presentation, or secret/configuration branches. Stable SSR markers, the exact
  persisted roll after reload, and a CSP-header/script nonce match are tested; add
  dedicated fixtures when any new dynamic branch is introduced.
- Correlation IDs now follow gameplay HTTP requests through server functions,
  application commands, transactions, and immutable turn audits. Generation jobs
  are not part of Slice 0 and will extend the chain when introduced.
- A green CI run still needs to be linked to the relevant checklist/PR before any
  checkbox is marked complete.

## Reviewer reproduction

```sh
docker compose up -d --wait postgres
export DATABASE_URL=postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/manchester_arcana

scripts/validate-migrations.sh
scripts/check-doc-links.sh
scripts/scan-secrets.sh

cargo leptos build --release --bin-cargo-args=--locked --lib-cargo-args=--locked
SMOKE_EVIDENCE_DIR="$PWD/target/slice-0-smoke" \
SMOKE_DATABASE_OUTAGE=1 scripts/provider-disabled-smoke.sh

npm ci --ignore-scripts --no-audit --no-fund
npx playwright install chromium firefox webkit
npm run test:browser
```

Do not attach retained smoke material until `scripts/scan-secrets.sh` has scanned it
with runtime canaries. Do not place real credentials in issue text, CI logs, test
reports, screenshots, or evidence archives.
