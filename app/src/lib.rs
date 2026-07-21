#![recursion_limit = "256"]

pub mod app;
#[cfg(feature = "ssr")]
pub(crate) mod auth_boundary;
pub(crate) mod components;
pub(crate) mod views;

pub use app::{App, shell};
pub(crate) use components::{campaign, freeform};
#[cfg(feature = "ssr")]
pub(crate) use views::home::authored_object_label;
pub(crate) use views::home::load_campaign_into;

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
