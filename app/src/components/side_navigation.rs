use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_location;

use crate::components::auth::logout;

#[component]
#[allow(dead_code)]
pub(crate) fn SideNavigation(#[prop(optional, into)] display_name: String) -> impl IntoView {
    let display_name = if display_name.trim().is_empty() {
        "Local player".to_owned()
    } else {
        display_name
    };
    let drawer_open = RwSignal::new(false);
    let location = use_location();

    view! {
        <aside class="side-navigation" data-testid="side-navigation">
            <a class="brand side-navigation-brand" href="/" aria-label="Manchester Arcana home">
                <span class="brand-mark" aria-hidden="true">"M"</span>
                <strong>"Manchester Arcana"</strong>
            </a>

            <button
                class="side-navigation-toggle"
                type="button"
                aria-expanded=move || drawer_open.get()
                aria-controls="side-nav-menu"
                on:click=move |_| drawer_open.update(|open| *open = !*open)
            >
                {move || if drawer_open.get() { "Close menu" } else { "Open menu" }}
            </button>

            <nav
                id="side-nav-menu"
                aria-label="Player navigation"
                class:side-nav-open=move || drawer_open.get()
                on:keydown=move |ev| {
                    if ev.key() == "Escape" && drawer_open.get() {
                        drawer_open.set(false);
                    }
                }
            >
                <a href="/" class:nav-active=move || location.pathname.get() == "/">
                    "Home"
                </a>
                <a href="/characters" class:nav-active=move || location.pathname.get().starts_with("/characters")>
                    "Characters"
                </a>
                <a href="/campaigns" class:nav-active=move || location.pathname.get().starts_with("/campaigns")>
                    "Campaigns"
                </a>
                <a href="/play" class:nav-active=move || location.pathname.get() == "/play">
                    "Play"
                </a>
                <a href="/guide" class:nav-active=move || location.pathname.get() == "/guide">
                    "Guide"
                </a>
                <a href="/privacy-and-safety" class:nav-active=move || location.pathname.get() == "/privacy-and-safety">
                    "Privacy & safety"
                </a>
                <a href="/legal" class:nav-active=move || location.pathname.get() == "/legal">
                    "Legal"
                </a>
            </nav>

            <div class="side-navigation-account">
                <span class="account-name">{display_name}</span>
                <LogoutButton/>
            </div>
        </aside>
        <div
            class="side-nav-backdrop"
            class:backdrop-visible=move || drawer_open.get()
            on:click=move |_| drawer_open.set(false)
            aria-hidden="true"
        ></div>
    }
}

#[component]
fn LogoutButton() -> impl IntoView {
    let pending = RwSignal::new(false);

    let on_logout = move |_| {
        if pending.get() {
            return;
        }
        pending.set(true);
        spawn_local(async move {
            let _ = logout().await;
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(win) = web_sys::window() {
                    if let Ok(Some(storage)) = win.local_storage() {
                        let _ = storage.remove_item("csrf");
                    }
                }
            }
            let _ = window().location().set_href("/");
        });
    };

    view! {
        <button
            type="button"
            class="logout-button"
            disabled=pending.get()
            on:click=on_logout
            aria-busy=pending.get()
        >
            {move || if pending.get() { "Signing out…" } else { "Logout" }}
        </button>
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;
    use leptos_router::components::Router;
    use leptos_router::location::RequestUrl;

    #[test]
    fn side_navigation_has_semantic_nav_and_all_links() {
        let owner = Owner::new();
        let html = owner.with(|| {
            provide_context(RequestUrl::new("/characters"));
            view! {
                <Router>
                    <SideNavigation display_name="Test Player"/>
                </Router>
            }
            .to_html()
        });
        assert!(html.contains(r#"data-testid="side-navigation""#));
        assert!(html.contains(r#"aria-label="Player navigation""#));
        assert!(html.contains("Characters"));
        assert!(html.contains("Campaigns"));
        assert!(html.contains("Play"));
        assert!(html.contains("Guide"));
        assert!(html.contains("Privacy"));
        assert!(html.contains("safety"));
        assert!(html.contains("Legal"));
        assert!(html.contains("Test Player"));
        assert!(html.contains("Logout"));
    }
}
