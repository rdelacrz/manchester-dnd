use leptos::prelude::*;

use super::public_header::{PublicFooter, PublicHeader};
use super::side_navigation::SideNavigation;

#[component]
pub(crate) fn PublicLayout(children: Children) -> impl IntoView {
    view! {
        <div class="public-layout" data-layout="public">
            <PublicHeader/>
            <main id="main-content" class="game-shell layout-content" tabindex="-1">
                {children()}
            </main>
            <PublicFooter/>
        </div>
    }
}

#[component]
pub(crate) fn AuthenticatedLayout(
    #[prop(optional, into)] display_name: String,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="authenticated-layout" data-layout="authenticated">
            <SideNavigation display_name/>
            <main id="main-content" class="authenticated-content" tabindex="-1">
                {children()}
            </main>
        </div>
    }
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;
    use leptos_router::components::Router;
    use leptos_router::location::RequestUrl;

    #[test]
    fn public_layout_has_no_player_navigation() {
        let owner = Owner::new();
        let html =
            owner.with(|| view! { <PublicLayout><h1>"Public"</h1></PublicLayout> }.to_html());
        assert!(html.contains("data-layout=\"public\""));
        assert!(html.contains("aria-label=\"Public navigation\""));
        assert!(html.contains("Log in"));
        assert!(html.contains("Sign up"));
        assert!(!html.contains("aria-label=\"Player navigation\""));
        assert!(!html.contains("data-testid=\"side-navigation\""));
    }

    #[test]
    fn authenticated_layout_has_semantic_player_navigation() {
        let owner = Owner::new();
        let html = owner.with(|| {
            provide_context(RequestUrl::new("/characters"));
            view! {
                <Router>
                    <AuthenticatedLayout display_name="Ada Lovelace">
                        <h1>"Characters"</h1>
                    </AuthenticatedLayout>
                </Router>
            }
            .to_html()
        });
        assert!(html.contains("data-layout=\"authenticated\""));
        assert!(html.contains("aria-label=\"Player navigation\""));
        assert!(html.contains("Ada Lovelace"));
        assert!(html.contains("Logout"));
        assert!(!html.contains("aria-label=\"Public navigation\""));
    }
}
