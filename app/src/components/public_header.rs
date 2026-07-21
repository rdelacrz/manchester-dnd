use leptos::prelude::*;

#[component]
pub(crate) fn PublicHeader() -> impl IntoView {
    view! {
        <header class="topbar public-header">
            <a class="brand" href="/" aria-label="Manchester Arcana home">
                <span class="brand-mark" aria-hidden="true">"M"</span>
                <span>
                    <strong>"Manchester Arcana"</strong>
                    <small>"An AI-guided 5E-compatible adventure"</small>
                </span>
            </a>
            <nav aria-label="Public navigation">
                <a href="/guide">"Guide"</a>
                <a href="/privacy-and-safety">"Privacy & safety"</a>
                <a href="/legal">"Legal"</a>
            </nav>
            <div class="public-auth-actions" aria-label="Account actions">
                <a href="/login">"Log in"</a>
                <a class="secondary-button" href="/signup">"Sign up"</a>
            </div>
            <div class="status-pill" role="status"><span></span>"Local campaign"</div>
        </header>
    }
}

#[component]
pub(crate) fn PublicFooter() -> impl IntoView {
    view! {
        <footer>
            <p>"Private evaluation build · Manchester Arcana is a working title."</p>
            <div class="footer-links">
                <a href="/guide">"Supported features"</a>
                <a href="/privacy-and-safety">"Privacy and reporting"</a>
                <a href="/legal">"Legal and attribution"</a>
            </div>
        </footer>
    }
}
