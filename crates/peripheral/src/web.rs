use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;

use crate::agent::{Agent, AgentEvent, AgentOutcome};
use crate::ipc_client;

use loopy_ipc::messages::{msg_types, Envelope};

const INDEX_HTML: &str = include_str!("static/index.html");

pub struct AppState {
    pub agent: Mutex<Agent>,
    pub ipc_tx: mpsc::Sender<Envelope>,
    pub update_result_rx: Mutex<mpsc::Receiver<Envelope>>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/chat", post(chat))
        .route("/api/reset", post(reset))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn reset(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut agent = state.agent.lock().await;
    agent.reset_conversation();
    "ok"
}

async fn chat(
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
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
            Ok(Ok(AgentOutcome::SubmitUpdate(source_path))) => {
                let submit = ipc_client::make_submit_update(&source_path);
                if state.ipc_tx.send(submit).await.is_err() {
                    let err_ev = AgentEvent::Error("Lost connection to Boot".into());
                    let _ = sse_tx
                        .send(Ok(Event::default().data(serde_json::to_string(&err_ev).unwrap())))
                        .await;
                    return;
                }

                let mut rx = state.update_result_rx.lock().await;
                match tokio::time::timeout(Duration::from_secs(300), rx.recv()).await {
                    Ok(Some(msg)) => {
                        let update_ev = build_update_event(&msg);
                        let _ = sse_tx
                            .send(Ok(Event::default().data(serde_json::to_string(&update_ev).unwrap())))
                            .await;

                        if msg.msg_type == msg_types::SHUTDOWN {
                            let _ = sse_tx
                                .send(Ok(Event::default().data(
                                    serde_json::to_string(&AgentEvent::Error(
                                        "Hot replacement in progress. Shutting down...".into(),
                                    ))
                                    .unwrap(),
                                )))
                                .await;
                        }
                    }
                    Ok(None) => {
                        let _ = sse_tx
                            .send(Ok(Event::default().data(
                                serde_json::to_string(&AgentEvent::Error("IPC channel closed".into())).unwrap(),
                            )))
                            .await;
                    }
                    Err(_) => {
                        let _ = sse_tx
                            .send(Ok(Event::default().data(
                                serde_json::to_string(&AgentEvent::Error(
                                    "Timed out waiting for build result".into(),
                                ))
                                .unwrap(),
                            )))
                            .await;
                    }
                }

                let mut agent = state.agent.lock().await;
                agent.source_mut().reset_staging();
            }
            Ok(Ok(AgentOutcome::Done)) => {}
            Ok(Err(e)) => {
                let err_ev = AgentEvent::Error(e);
                let _ = sse_tx
                    .send(Ok(Event::default().data(serde_json::to_string(&err_ev).unwrap())))
                    .await;
            }
            Err(e) => {
                let err_ev = AgentEvent::Error(format!("Agent task panicked: {}", e));
                let _ = sse_tx
                    .send(Ok(Event::default().data(serde_json::to_string(&err_ev).unwrap())))
                    .await;
            }
        }
    });

    Sse::new(ReceiverStream::new(sse_rx)).keep_alive(KeepAlive::default())
}

fn build_update_event(envelope: &Envelope) -> AgentEvent {
    match envelope.msg_type.as_str() {
        msg_types::UPDATE_ACCEPTED => {
            let version = envelope
                .payload
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            AgentEvent::Content(format!("\n**Update ACCEPTED** — version {} deployed\n", version))
        }
        msg_types::UPDATE_REJECTED => {
            let reason = envelope
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            AgentEvent::Error(format!("Update REJECTED: {}", reason))
        }
        _ => AgentEvent::Error(format!("Unexpected message: {}", envelope.msg_type)),
    }
}
