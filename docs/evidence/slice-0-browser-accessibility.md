# Slice 0 browser and accessibility evidence contract

Status date: 2026-07-14. This document turns the accepted
[Q02 policy](../planning/12-mvp-policy-resolutions.md) into an executable matrix and
separates automated engine coverage from manual operating-system and real-device
claims.

## Exact supported-client policy

- Desktop: current and previous major stable Chromium and Firefox releases on
  Windows, macOS, and Linux, at `1440×900` CSS pixels or larger.
- Desktop Safari: current stable Safari on macOS at `1440×900` or larger.
- Mobile: current stable Safari on iOS and Chrome on Android at `390×844` CSS
  pixels or larger.
- Accessibility: WCAG 2.2 AA target.
- Language: English (`en`) only for MVP.
- Time: persist UTC instants; when time is shown, label the user's local time and
  make UTC available explicitly. Slice 0 currently renders no player-facing time,
  so this branch has no browser evidence yet.
- Unsupported clients must receive usable SSR HTML rather than a blank shell.

The client contract is reviewed at every private release. “Current” and “previous”
refer to stable releases on the release-test date, not versions recorded in this
document months earlier.

## Pinned automated matrix

`package-lock.json` pins Playwright 1.61.1. Its downloaded CI revisions are Chromium
149.0.7827.55 (revision 1228), Firefox 151.0 (revision 1532), and WebKit 26.5
(revision 2311). `tests/browser/config/playwright.config.ts` runs every check below serially against:

| Project | Engine and viewport | What it proves | What it does not prove |
| --- | --- | --- | --- |
| `chromium-desktop-1440x900` | Pinned Chromium, `1440×900` | Linux CI engine SSR/hydration behavior | Chrome on Windows/macOS or both current/previous stable releases |
| `firefox-desktop-1440x900` | Pinned Firefox, `1440×900` | Linux CI engine SSR/hydration behavior | Firefox on Windows/macOS or both current/previous stable releases |
| `webkit-desktop-1440x900` | Pinned WebKit, `1440×900` | WebKit compatibility signal | Shipping Safari on macOS |
| `chromium-android-emulation-390x844` | Pixel 7 profile with exact `390×844` viewport | Chromium mobile emulation and responsive layout | A physical Android device, OEM browser integration, or touch hardware |
| `firefox-responsive-390x844` | Pinned Firefox with touch and `390×844` viewport | Extra responsive-engine regression coverage | A supported Firefox mobile product claim |
| `webkit-ios-emulation-390x844` | iPhone 14 profile with exact `390×844` viewport | WebKit mobile emulation and responsive layout | Mobile Safari on physical iOS hardware |

CI installs the exact browser binaries from the pinned package; it does not use
unversioned system browser channels. A green Playwright artifact is automated
evidence for these six projects only.

## Per-project automated assertions

The browser suite performs the following on each project:

1. Fetch the production SSR document and require English, the title, the level-one
   heading, and a gameplay marker before hydration.
2. Require the per-response CSP nonce to match every inline hydration/resource
   script, load the release WASM, wait for the database-backed campaign state,
   preserve stable SSR/client shell text, and fail on every console warning,
   console error, page error, or hydration warning.
3. With text and image providers disabled, submit the existing non-AI exploration
   action, reload, and require the byte-identical stored roll summary rather than a
   reroll.
4. Create a JavaScript-disabled browser context, require useful SSR content, and
   submit the theme selector as a native GET. The gameplay mutation remains
   disabled because its dice and persistence require the authoritative server
   function; after hydration the same theme values preview without navigation.
5. Run axe-core rules tagged WCAG 2 A/AA, WCAG 2.1 AA, and WCAG 2.2 AA after
   hydration.
6. Require a visible keyboard focus target after `Tab`, the reduced-motion media
   query to suppress the normal transition, no button below `24×24` CSS pixels, and
   no horizontal viewport overflow.

CI uploads `target/playwright/report/` as `slice-0-playwright-report` for 14 days. The
report becomes evidence only after a green run for the commit under review.

Local execution on 2026-07-14 passed all four scenarios in Chromium desktop and
Android emulation and Firefox desktop and responsive mobile (16 tests total), with
the CSP nonce fix included. The local host lacked Playwright's WebKit shared-library
dependencies, so the two WebKit projects remain configured for the CI image's
`playwright install --with-deps` run and are not claimed as locally verified.

## Required real-platform release matrix

These rows are manual because Linux Playwright emulation is not an operating-system,
assistive-technology, or physical-device claim. Record browser versions, OS/device
versions, build commit, tester, date, and an evidence link.

| Required release pass | Viewport/device | Status on 2026-07-14 |
| --- | --- | --- |
| Current Chrome and previous Chrome on Windows | Desktop `1440×900`, keyboard and 200%/400% zoom | Not run |
| Current Firefox and previous Firefox on Windows | Desktop `1440×900`, keyboard and NVDA | Not run |
| Current Chrome and previous Chrome on macOS | Desktop `1440×900`, keyboard and zoom | Not run |
| Current Firefox and previous Firefox on macOS | Desktop `1440×900`, keyboard and zoom | Not run |
| Current Safari on macOS | Desktop `1440×900`, keyboard and VoiceOver | Not run |
| Current/previous Chromium and Firefox on Linux | Desktop `1440×900`, keyboard and zoom | Playwright engine automation configured; stable-channel manual pass not run |
| Current Mobile Safari on physical iOS | At least `390×844`, touch and VoiceOver | Not run |
| Current Chrome on physical Android | At least `390×844`, touch and TalkBack | Not run |

For every row, exercise initial load, hydration, action commit, reload, keyboard or
touch navigation, focus recovery after an error, 200% and 400% zoom/reflow where
applicable, high-contrast/forced-colors where available, reduced motion, slow
network, and loss/recovery of the server connection.

## Accessibility evidence ledger

| Baseline | Automated evidence | Manual evidence | Current status |
| --- | --- | --- | --- |
| Semantics, names, roles, language, common contrast rules | axe-core in all six projects | Screen-reader landmark/heading/control review | Automation configured; manual not run |
| Keyboard order and visible focus | First `Tab` receives `:focus-visible` | Complete keyboard-only action/error/reload pass | Basic automation configured; manual not run |
| Screen readers | None can substitute for AT | NVDA/Firefox Windows, VoiceOver/Safari macOS+iOS, TalkBack/Chrome Android | Not run |
| Zoom and reflow | `390×844` overflow guard | 200% and 400% browser zoom with text spacing | Responsive automation configured; manual not run |
| Contrast and forced colors | axe-core detectable contrast rules | Visual state/disabled/focus/high-contrast review | Automation configured; manual not run |
| Reduced motion | Emulated preference suppresses transitions | OS/browser preference with all gameplay states | Basic automation configured; manual not run |
| Touch targets | Buttons at least WCAG 2.2 minimum `24×24` | Physical-device accuracy, spacing, gestures | Basic automation configured; manual not run |
| Loading and errors | Live region is included in axe scan | Screen-reader announcement timing and focus recovery | Manual not run |

## Known evidence gaps

- The progressively enhanced theme form proves no-WASM submission for character
  presentation. Authoritative gameplay mutations intentionally do not fall back to
  client-computed or query-string mechanics.
- No timestamp is displayed, so local-time/UTC presentation cannot be tested.
- The shell has no server/client timestamp, randomized presentation, or
  configuration-derived UI branch. Tests protect current stable markers and stored
  roll reload, but dedicated deterministic hydration fixtures are still needed when
  those values appear.
- Playwright is not a real Safari, iOS, Android, Windows, macOS, screen-reader,
  browser-zoom, or slow-network certification.
- Automated axe checks find only a subset of WCAG failures. The WCAG 2.2 AA target
  is not complete until the manual ledger and issue remediation are complete.
