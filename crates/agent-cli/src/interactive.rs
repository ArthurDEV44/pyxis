//! Boucle interactive : assemble le frontend (`agent-tui`), le stream d'agent
//! (`agent-core`) et les demandes de permission en un `tokio::select`.
//!
//! - Les frappes clavier arrivent d'un thread dédié (crossterm `read()` bloque).
//! - Chaque soumission spawn `run_agent` ; ses `AgentEvent` reviennent par mpsc.
//! - Une demande de permission suspend le pipeline d'outils jusqu'à la réponse
//!   utilisateur (le dialog ne fige PAS la boucle : le select continue de rendre
//!   et de lire le clavier).

use std::sync::{Arc, Mutex};

use agent_core::message::Message;
use agent_core::provider::ToolSpec;
use agent_core::{AgentContext, AgentEvent, Deps, RunConfig, run_agent};
use agent_tui::{AppState, Block, InputAction};
use crossterm::event::{Event, KeyEventKind};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};

use crate::approver::{PermissionMsg, to_prompt};

pub struct InteractiveConfig {
    pub model: String,
    pub system: String,
    pub run_config: RunConfig,
    pub tool_specs: Vec<ToolSpec>,
    pub truecolor: bool,
}

/// Lance la session interactive. Restaure le terminal en sortie quoi qu'il arrive.
pub async fn run(
    deps: Deps,
    conversation: Arc<Mutex<Vec<Message>>>,
    perm_rx: mpsc::Receiver<PermissionMsg>,
    cfg: InteractiveConfig,
) -> anyhow::Result<()> {
    let mut tui = agent_tui::enter()?;
    let result = event_loop(&mut tui, deps, conversation, perm_rx, cfg).await;
    agent_tui::leave(&mut tui)?;
    result
}

async fn event_loop(
    tui: &mut agent_tui::Tui,
    deps: Deps,
    conversation: Arc<Mutex<Vec<Message>>>,
    mut perm_rx: mpsc::Receiver<PermissionMsg>,
    cfg: InteractiveConfig,
) -> anyhow::Result<()> {
    let mut state = AppState::new(cfg.model.clone(), cfg.truecolor);
    state.blocks.push(Block::Notice(
        "Numen — tape ta demande, ⌃C pour quitter".into(),
    ));

    // Thread lecteur clavier → mpsc (crossterm read() est bloquant).
    let (key_tx, mut key_rx) = mpsc::channel::<Event>(64);
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if key_tx.blocking_send(ev).is_err() {
                break;
            }
        }
    });

    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(256);
    let mut running = false;
    let mut pending_resp: Option<oneshot::Sender<bool>> = None;

    loop {
        tui.draw(|f| agent_tui::render(f, &state))?;
        if state.should_quit {
            break;
        }

        tokio::select! {
            key = key_rx.recv() => {
                let Some(Event::Key(k)) = key else {
                    // canal clavier fermé → on sort ; resize/autres events : redraw.
                    if key.is_none() { break; }
                    continue;
                };
                if k.kind == KeyEventKind::Release {
                    continue;
                }
                match state.on_key(k) {
                    InputAction::Submit(prompt) if !running => {
                        state.push_user(prompt.clone());
                        let mut msgs = conversation.lock().map(|g| g.clone()).unwrap_or_default();
                        msgs.push(Message::user(prompt));
                        let ctx = AgentContext {
                            model: cfg.model.clone(),
                            system: Some(cfg.system.clone()),
                            messages: msgs,
                            tools: cfg.tool_specs.clone(),
                            config: cfg.run_config.clone(),
                        };
                        let d = deps.clone();
                        let tx = agent_tx.clone();
                        running = true;
                        tokio::spawn(async move {
                            let stream = run_agent(ctx, d);
                            futures_util::pin_mut!(stream);
                            while let Some(ev) = stream.next().await {
                                if tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    InputAction::Quit => state.should_quit = true,
                    InputAction::Permission(allow) => {
                        if let Some(resp) = pending_resp.take() {
                            let _ = resp.send(allow);
                        }
                    }
                    _ => {}
                }
            }
            ev = agent_rx.recv(), if running => {
                if let Some(ev) = ev {
                    let end = matches!(
                        ev,
                        AgentEvent::EndTurn | AgentEvent::Error(_) | AgentEvent::Exhausted(_)
                    );
                    state.apply(&ev);
                    if end {
                        running = false;
                    }
                }
            }
            perm = perm_rx.recv() => {
                if let Some((req, resp)) = perm {
                    state.pending = Some(to_prompt(&req));
                    pending_resp = Some(resp);
                }
            }
        }
    }
    Ok(())
}
