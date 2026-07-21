use leptos::prelude::*;
use serde::{Deserialize, Serialize};

/// Displays recent turn history for a campaign play session.
#[component]
pub fn TurnHistory(campaign_id: String) -> impl IntoView {
    let history = Resource::new(
        move || campaign_id.clone(),
        |id| async move { load_turn_history(id).await },
    );

    view! {
        <section class="turn-history">
            <h2>"Turn History"</h2>
            <Suspense fallback=|| {
                view! { <p>"Loading history…"</p> }
            }>
                {move || {
                    let h = match history.get() {
                        Some(v) => v,
                        None => return view! { <p>"Loading…"</p> }.into_any(),
                    };
                    match h {
                        Ok(entries) if entries.is_empty() => {
                            view! { <p class="history-empty">"No turns recorded yet."</p> }
                                .into_any()
                        }
                        Ok(entries) => view! {
                            <ol class="history-list">
                                {entries.iter().map(|e| {
                                    view! {
                                        <li class="history-entry">
                                            <span class="history-round">
                                                "Round "{e.round}" / Turn "{e.turn_number}
                                            </span>
                                            <span class="history-actor">{e.actor_name.clone()}</span>
                                            <span class="history-action">{e.action_summary.clone()}</span>
                                            {e.outcome.as_ref().map(|o| {
                                                view! {
                                                    <span class="history-outcome">{o.clone()}</span>
                                                }
                                            })}
                                        </li>
                                    }
                                }).collect::<Vec<_>>()}
                            </ol>
                        }
                            .into_any(),
                        Err(e) => view! {
                            <p class="history-error">"Failed to load history: "{e.to_string()}</p>
                        }
                            .into_any(),
                    }
                }}
            </Suspense>
        </section>
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TurnHistoryEntry {
    pub round: i32,
    pub turn_number: i32,
    pub actor_name: String,
    pub action_summary: String,
    pub outcome: Option<String>,
}

#[server(LoadTurnHistory)]
async fn load_turn_history(campaign_id: String) -> Result<Vec<TurnHistoryEntry>, ServerFnError> {
    // TODO: Implement server-side turn history loading via the lobby application service.
    let _ = campaign_id;
    Ok(Vec::new())
}
