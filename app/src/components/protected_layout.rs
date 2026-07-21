use leptos::prelude::*;

use crate::components::auth::{AuthStateView, current_auth_state};
use crate::components::layout::AuthenticatedLayout;

/// Protected layout that enforces authentication on the client side.
/// Server-side authorization remains mandatory even if this guard is bypassed.
///
/// In local mode, the compatibility principal is always authenticated.
/// In hosted mode, a valid session cookie is required.
#[component]
pub(crate) fn ProtectedLayout(children: Children) -> impl IntoView {
    view! {
        <Suspense fallback=|| {
            view! {
                <div class="auth-loading" role="status" aria-live="polite">
                    "Loading your session…"
                </div>
            }
        }>
            <ProtectedGate children/>
        </Suspense>
    }
}

/// Inner gate that resolves the auth state once and either redirects
/// or renders the authenticated shell.
#[component]
fn ProtectedGate(children: Children) -> impl IntoView {
    let auth_state = current_auth_state();

    view! {
        <Await future=auth_state let:result>
            {match result {
                Ok(AuthStateView { authenticated: false, .. }) | Err(_) => {
                    let next = current_path();
                    view! {
                        <leptos_router::components::Redirect
                            path=format!("/login?next={next}")
                        />
                    }.into_any()
                }
                Ok(AuthStateView { display_name, .. }) => {
                    let name = display_name.clone().unwrap_or_else(|| "Local player".to_owned());
                    view! {
                        <AuthenticatedLayout display_name=name>
                            {children()}
                        </AuthenticatedLayout>
                    }.into_any()
                }
            }}
        </Await>
    }
}

fn current_path() -> String {
    #[cfg(feature = "ssr")]
    {
        if let Some(parts) = use_context::<axum::http::request::Parts>() {
            return encode_path(parts.uri.path());
        }
        String::new()
    }
    #[cfg(not(feature = "ssr"))]
    {
        let path = window().location().pathname().unwrap_or_default();
        encode_path(&path)
    }
}

/// Percent-encodes a path for use in a `next` query parameter.
/// Only encodes characters that are unsafe in a URL query value.
fn encode_path(path: &str) -> String {
    path.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::AuthenticatedLayout;
    use leptos::prelude::*;
    use leptos_router::components::Router;
    use leptos_router::location::RequestUrl;

    #[test]
    fn authenticated_shell_renders_side_navigation_and_content() {
        let owner = Owner::new();
        let html = owner.with(|| {
            provide_context(RequestUrl::new("/characters"));
            view! {
                <Router>
                    <AuthenticatedLayout display_name="Test Player">
                        <h1>"Characters"</h1>
                    </AuthenticatedLayout>
                </Router>
            }
            .to_html()
        });
        assert!(html.contains(r#"data-layout="authenticated""#));
        assert!(html.contains(r#"aria-label="Player navigation""#));
        assert!(html.contains("Test Player"));
        assert!(html.contains("Logout"));
        assert!(html.contains("/characters"));
        assert!(html.contains("/campaigns"));
    }

    #[test]
    fn side_navigation_has_semantic_nav_and_all_links() {
        let owner = Owner::new();
        let html = owner.with(|| {
            provide_context(RequestUrl::new("/characters"));
            view! {
                <Router>
                    <crate::components::side_navigation::SideNavigation display_name="Test Player"/>
                </Router>
            }
            .to_html()
        });
        assert!(html.contains(r#"aria-label="Player navigation""#));
        assert!(html.contains("Characters"));
        assert!(html.contains("Campaigns"));
        assert!(html.contains("Guide"));
        assert!(html.contains("Privacy"));
        assert!(html.contains("safety"));
        assert!(html.contains("Legal"));
        assert!(html.contains("Logout"));
    }
}
