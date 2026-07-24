use leptos::prelude::*;
use leptos_meta::Title;

use crate::components::layout::PublicLayout;

#[component]
pub(crate) fn GuidePage() -> impl IntoView {
    view! {
        <Title text="Guide and supported features · Manchester Arcana"/>
        <PublicLayout>
            <div class="info-shell">
            <section class="info-hero" aria-labelledby="guide-heading">
                <p class="eyebrow">"START HERE"</p>
                <h1 id="guide-heading">"Set up safely, then follow the saved story"</h1>
                <p class="lede">
                    "This build is a loopback-only, single-player private evaluation. It is not a hosted service and must not be exposed through a proxy or public network."
                </p>
            </section>

            <section class="info-grid" aria-label="First-run guide">
                <article class="panel info-card">
                    <h2>"Before the first turn"</h2>
                    <ul>
                        <li>"Keep the server bound to 127.0.0.1 or ::1. Another process on the same computer is outside this build's trust boundary."</li>
                        <li>"Leave text, image, and private-inspiration providers disabled unless the private-test operator has approved their terms and configuration."</li>
                        <li>"Do not add real-life source material until every represented participant has been verified and has granted scoped consent out of band."</li>
                        <li>"Back up MongoDB and the protected RNG/image/source keys with the encrypted recovery procedure before relying on a campaign save. DragonflyDB is disposable and excluded from backups."</li>
                    </ul>
                </article>

                <article class="panel info-card">
                    <h2>"Your first run"</h2>
                    <ol class="first-run-list">
                        <li><a href="/play#campaigns">"Create or resume the local campaign"</a>" and confirm its saved revision."</li>
                        <li><a href="/play#themes">"Build a rules-valid hero"</a>"; every step is saved and can survive a refresh."</li>
                        <li><a href="/play#play">"Inspect the viaduct runes"</a>" to commit the authored exploration consequence."</li>
                        <li><a href="/play#encounter">"Choose only rendered legal actions"</a>" until victory or the non-terminal story-recovery defeat."</li>
                        <li>"Claim trusted encounter XP, advance to level 2, inspect stored history, and make a private export from the campaign library."</li>
                    </ol>
                </article>

                <article class="panel info-card">
                    <h2>"Supported MVP surface"</h2>
                    <p>"The exposed rules are deliberately closed: human fighter or wizard, levels 1–2, the listed backgrounds/equipment/spells, one authored Soot Wight encounter, and two presentation-only themes. Unlisted mechanics are unavailable rather than improvised."</p>
                    <p>"The supported browser policy is the current and previous stable Chromium and Firefox on desktop, current Safari on macOS and iOS, and current Chrome on Android at 390×844 CSS pixels or larger. English is the only supported language; stored instants are UTC."</p>
                    <p>"CI exercises pinned Chromium, Firefox, and WebKit desktop/mobile profiles. Real Safari/iOS, Chrome/Android, and assistive-technology passes remain manual release evidence."</p>
                </article>

                <article class="panel info-card">
                    <h2>"Known limits and degraded play"</h2>
                    <ul>
                        <li>"Hosted accounts, multiplayer, public sharing, uploads, portraits, maps, voice, and broad level 1–20 rules are unavailable."</li>
                        <li>"If a text provider is disabled, slow, malformed, unsafe, or unavailable, committed mechanics remain saved and authored narration is shown."</li>
                        <li>"Scene images are optional and never block a turn. Only manually requested, verified private variants can be displayed."</li>
                        <li>"A database outage leaves liveness available but prevents safe play until readiness recovers; the UI must never invent a successful save."</li>
                    </ul>
                </article>
            </section>

            <section class="info-callout" aria-labelledby="need-help-heading">
                <h2 id="need-help-heading">"Need to stop or report something?"</h2>
                <p>"Pause play, disable providers or private inspiration when relevant, preserve only the safe correlation code and time, and use the private-test reporting route. Never paste campaign prose, source material, credentials, or exported saves into a report."</p>
                <a class="primary-button" href="/privacy-and-safety">"Privacy controls and reporting"</a>
            </section>
            </div>
        </PublicLayout>
    }
}

#[component]
pub(crate) fn PrivacyAndSafetyPage() -> impl IntoView {
    view! {
        <Title text="Privacy, safety, and reporting · Manchester Arcana"/>
        <PublicLayout>
            <div class="info-shell">
            <section class="info-hero" aria-labelledby="privacy-heading">
                <p class="eyebrow">"PRIVATE BY DEFAULT"</p>
                <h1 id="privacy-heading">"Your campaign and real-life inspiration stay under your control"</h1>
                <p class="lede">"The private MVP has no public campaign links, behavioral analytics, or hosted accounts. Optional providers and real-life inspiration are disabled by default."</p>
            </section>

            <section class="info-grid">
                <article class="panel info-card">
                    <h2>"What this build retains"</h2>
                    <p>"MongoDB retains revisioned campaign and hero state, immutable mechanical audits, body-free command receipts, selected presentation artifacts, and bounded operational metadata. DragonflyDB contains only short-lived reconstructable cache data. Raw provider prompts, credentials, RNG key material, and raw private-source bodies do not belong in ordinary game state or logs."</p>
                    <p>"Active campaigns and selected artifacts remain until owner deletion. Incomplete drafts, failed jobs, superseded presentations, diagnostics, exports, backups, and deletion tombstones use the bounded retention periods documented for the private test."</p>
                </article>

                <article class="panel info-card">
                    <h2>"Private-inspiration controls"</h2>
                    <p>"Consent is source-, campaign-, audience-, medium-, transformation-, and expiry-specific. Missing or revoked consent is always ineligible. The game exposes pause, veil, source veto, category veto, disable-all, and privacy-report actions without requiring a reason."</p>
                    <p>"A veto hides the current presentation and cancels related pending work without rewriting saved dice or mechanics. Revocation and deletion continue through the documented backup-expiry window."</p>
                </article>

                <article class="panel info-card">
                    <h2>"Operational telemetry"</h2>
                    <p>"The accepted policy permits only bounded operational counts, latency, health, queue, cost, fallback, and denial signals. It forbids behavioral funnels and campaign text, prompts, source bodies, identities, dice seeds, or generated bodies as metric labels or analytics payloads."</p>
                </article>

                <article class="panel info-card" id="reporting">
                    <h2>"Report a security, privacy, or safety issue"</h2>
                    <p>"Stop using the affected feature and contact the operator who issued your private-test invitation through that established out-of-band channel. If that channel is unavailable, stop the server and retain the encrypted local evidence until the operator provides a secure route."</p>
                    <p>"Share only the build revision, approximate UTC time, safe public error code or correlation ID, affected feature, and impact. Do not send campaign text, names, source material, screenshots containing private content, database dumps, exports, keys, tokens, or provider responses."</p>
                    <p>"For an urgent consent issue, use the in-game pause or disable control first. The private-test operator follows the consent incident, credential rotation, quarantine, notification, deletion, and recovery runbook."</p>
                </article>
            </section>
            <div class="info-actions">
                <a class="primary-button" href="/play#privacy">"Open in-game privacy controls"</a>
                <a class="text-link" href="/legal">"Read legal and attribution information →"</a>
            </div>
            </div>
        </PublicLayout>
    }
}

#[component]
pub(crate) fn LegalPage() -> impl IntoView {
    view! {
        <Title text="Legal and attribution · Manchester Arcana"/>
        <PublicLayout>
            <div class="info-shell">
            <section class="info-hero" aria-labelledby="legal-heading">
                <p class="eyebrow">"ATTRIBUTION AND DISTRIBUTION"</p>
                <h1 id="legal-heading">"Private evaluation notices"</h1>
                <p class="lede">"Manchester Arcana is a private working title. This build grants no public code, original-content, or content-pack license and is not cleared for public distribution."</p>
            </section>

            <section class="info-grid">
                <article class="panel info-card legal-notice">
                    <h2>"System Reference Document 5.1"</h2>
                    <blockquote>
                        "This work includes material taken from the System Reference Document 5.1 (“SRD 5.1”) by Wizards of the Coast LLC and available at https://dnd.wizards.com/resources/systems-reference-document. The SRD 5.1 is licensed under the Creative Commons Attribution 4.0 International License available at https://creativecommons.org/licenses/by/4.0/legalcode."
                    </blockquote>
                    <p>"The software rewrites procedures as typed data and deterministic functions. Original setting material is kept separate. The project does not use excluded Basic Rules text, Wizards of the Coast logos, protected settings, or non-SRD product identity."</p>
                    <p><a href="https://creativecommons.org/licenses/by/4.0/legalcode" rel="noreferrer">"Creative Commons Attribution 4.0 legal code"</a></p>
                </article>

                <article class="panel info-card">
                    <h2>"Private-evaluation constraint"</h2>
                    <p>"All rights in original code and content remain reserved by their respective owners. Outside contributions and third-party pack intake are closed. A public repository, public artifact, domain launch, or marketing release is blocked until explicit code/content licenses, contribution terms, and external name/domain/trademark clearance are recorded."</p>
                </article>

                <article class="panel info-card">
                    <h2>"Generated material"</h2>
                    <p>"No real text or image provider is approved merely because an adapter exists. Each enabled deployment profile requires a recorded review of retention, training, region, deletion, moderation, output rights, similarity, likeness, and takedown terms. Generated material is never promoted into an immutable content pack without human rewrite and rights, safety, and provenance review."</p>
                </article>

                <article class="panel info-card">
                    <h2>"Branding"</h2>
                    <p>"“Manchester Arcana” is a private working title and does not claim endorsement by Wizards of the Coast, the city of Manchester, any business, or any real person. Public name, domain, and trademark clearance remains an explicit release blocker."</p>
                </article>
            </section>
            </div>
        </PublicLayout>
    }
}
