use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::{Agent, AgentEvent, AgentOutcome};

const INDEX_HTML: &str = include_str!("static/index.html");

pub struct AppState {
    pub agent: Mutex<Agent>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/chat", post(chat))
        .route("/api/reset", post(reset))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
            (header::PRAGMA, "no-cache"),
        ],
        Html(INDEX_HTML),
    )
}

async fn reset(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut agent = state.agent.lock().await;
    agent.reset_conversation();
    "ok"
}

async fn chat(State(state): State<Arc<AppState>>, body: String) -> impl IntoResponse {
    let (sse_tx, sse_rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(256);

    tokio::spawn(async move {
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

        let agent_state = state.clone();
        let input = body.clone();
        let agent_handle = tokio::spawn(async move {
            let mut agent = agent_state.agent.lock().await;
            agent.handle_input_stream(&input, event_tx).await
        });

        while let Some(ev) = event_rx.recv().await {
            let json = match serde_json::to_string(&ev) {
                Ok(j) => j,
                Err(_) => continue,
            };
            let event = Event::default().data(json);
            if sse_tx.send(Ok(event)).await.is_err() {
                return;
            }
        }

        match agent_handle.await {
            Ok(Ok(AgentOutcome::Done)) => {}
            Ok(Err(e)) => {
                let err_ev = AgentEvent::Error(e);
                let _ = sse_tx
                    .send(Ok(
                        Event::default().data(serde_json::to_string(&err_ev).unwrap())
                    ))
                    .await;
            }
            Err(e) => {
                let err_ev = AgentEvent::Error(format!("Agent task panicked: {}", e));
                let _ = sse_tx
                    .send(Ok(
                        Event::default().data(serde_json::to_string(&err_ev).unwrap())
                    ))
                    .await;
            }
        }
    });

    Sse::new(ReceiverStream::new(sse_rx)).keep_alive(KeepAlive::default())
}
