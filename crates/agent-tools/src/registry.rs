//! `Registry` — implémente `ToolDispatch` (la frontière cœur↔outils). Partitionne
//! un batch en concurrent (read-only safe, `buffer_unordered(10)`) vs série
//! (mutants), et fait passer chaque appel par le **pipeline strict** (§4.3) :
//!
//! ```text
//! parse+validate → permission(mode×taint) → call() sous timeout → taint → outcome
//! ```
//!
//! Invariants : un `ToolOutcome` par `ToolInvocation` (même refus/inconnu/parse
//! KO → outcome d'erreur, jamais de panic, corrélation par `id`), fail-closed
//! partout.

use std::collections::HashMap;
use std::sync::Arc;

use agent_core::event::PermissionReq;
use agent_core::provider::ToolSpec;
use agent_core::tools::{
    ToolDispatch, ToolDispatchEvent, ToolEventSink, ToolInvocation, ToolOutcome,
};
use async_trait::async_trait;
use futures_util::stream::{self, StreamExt};

use crate::permission::{
    Approver, AutoDeny, PermCtx, PermissionMode, PermissionRequest, Resolved, resolve_permission,
};
use crate::taint::TaintTracker;
use crate::tool::{DynTool, ToolCtx, into_dyn};

/// Description d'outil cappée à l'exposition (un outil ne pollue pas le prompt).
const MAX_DESCRIPTION: usize = 2048;
/// Plafond de concurrence du batch read-only (ARCHITECTURE §4.2).
const CONCURRENCY: usize = 10;

/// Registre d'outils + politique d'exécution. Construit par la CLI/TUI, injecté
/// dans le cœur comme `Arc<dyn ToolDispatch>`.
pub struct Registry {
    tools: HashMap<String, Box<dyn DynTool>>,
    mode: PermissionMode,
    approver: Arc<dyn Approver>,
    taint: TaintTracker,
    ctx: ToolCtx,
}

impl Registry {
    pub fn builder(workspace: impl Into<std::path::PathBuf>) -> RegistryBuilder {
        RegistryBuilder {
            tools: HashMap::new(),
            mode: PermissionMode::default(),
            approver: None,
            taint_window: crate::taint::DEFAULT_WINDOW,
            ctx: ToolCtx::new(workspace),
        }
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// Le taint est-il récent ? (exposé pour les tests / l'observabilité.)
    pub fn taint_recent(&self) -> bool {
        self.taint.is_recent()
    }

    /// Specs exposées au modèle (descriptions cappées), pour `AgentContext.tools`.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .values()
            .map(|t| {
                let mut description = t.description();
                if description.len() > MAX_DESCRIPTION {
                    description.truncate(MAX_DESCRIPTION);
                }
                ToolSpec {
                    name: t.name().to_string(),
                    description,
                    input_schema: t.input_schema(),
                }
            })
            .collect();
        // Ordre stable (déterminisme du prompt / des tests).
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Collecte les guidelines comportementales de tous les outils (US-026), pour
    /// injection dans le system prompt. Ordre déterministe (tri par nom d'outil,
    /// guidelines de l'outil dans l'ordre de déclaration) → prompt stable et
    /// cache-friendly.
    pub fn behavioral_guidelines(&self) -> Vec<String> {
        let mut names: Vec<&String> = self.tools.keys().collect();
        names.sort();
        let mut out = Vec::new();
        for n in names {
            if let Some(t) = self.tools.get(n) {
                for g in t.behavioral_guidelines() {
                    out.push((*g).to_string());
                }
            }
        }
        out
    }

    /// Chemin de confort pour les tests et appels directs au registre.
    pub async fn dispatch(&self, calls: Vec<ToolInvocation>) -> Vec<ToolOutcome> {
        <Self as ToolDispatch>::dispatch(self, calls, ToolEventSink::default()).await
    }

    /// Pipeline strict d'un seul appel. Ne panique jamais : retourne toujours un
    /// `ToolOutcome` corrélé par `id`.
    async fn run_one(&self, call: ToolInvocation, events: ToolEventSink) -> ToolOutcome {
        let id = call.id.clone();
        let Some(tool) = self.tools.get(&call.name) else {
            return err_outcome(id, format!("outil inconnu: {}", call.name));
        };

        // 1. parse + validate (fail-closed, US-010 AC3) — pas d'exécution si KO.
        if let Err(e) = tool.precheck(&call.input) {
            return err_outcome(id, e.to_string());
        }

        // 2. permission : baseline outil mise en forme par mode + taint (§4.4/4.6).
        let pctx = PermCtx {
            mode: self.mode,
            taint_recent: self.taint.is_recent(),
        };
        let baseline = tool.permission(&call.input, &pctx);
        let resolved = resolve_permission(
            self.mode,
            baseline,
            tool.is_read_only(),
            tool.is_sensitive(),
            pctx.taint_recent,
        );
        match resolved {
            Resolved::Deny => {
                return err_outcome(
                    id,
                    format!(
                        "permission refusée pour « {} » (mode {:?})",
                        call.name, self.mode
                    ),
                );
            }
            Resolved::Ask => {
                let req = PermissionRequest {
                    tool: call.name.clone(),
                    reason: ask_reason(pctx.taint_recent, tool.is_sensitive()),
                    input_summary: summarize(&call.input),
                    input: call.input.clone(),
                };
                events.emit(ToolDispatchEvent::PermissionAsk(PermissionReq {
                    call_id: id.clone(),
                    tool: req.tool.clone(),
                    reason: req.reason.clone(),
                    input_summary: req.input_summary.clone(),
                    input: req.input.clone(),
                    mode: format!("{:?}", self.mode),
                }));
                if !self.approver.approve(&req).await {
                    return err_outcome(
                        id,
                        format!("action « {} » refusée par l'utilisateur", call.name),
                    );
                }
            }
            Resolved::Allow => {}
        }

        // 3. call() sous timeout (un outil qui pend ne bloque pas la boucle).
        let untrusted = tool.returns_untrusted();
        match tokio::time::timeout(self.ctx.timeout, tool.invoke(call.input, &self.ctx)).await {
            Err(_elapsed) => {
                if untrusted {
                    self.taint.mark();
                }
                err_outcome_tainted(id, "timeout outil dépassé".to_string(), untrusted)
            }
            Ok(Err(e)) => {
                if untrusted {
                    self.taint.mark();
                }
                err_outcome_tainted(id, e.to_string(), untrusted)
            }
            Ok(Ok(out)) => {
                // 4. taint : une sortie untrusted vient d'entrer dans le contexte.
                if untrusted {
                    self.taint.mark();
                }
                ToolOutcome {
                    id,
                    content: out.content,
                    is_error: out.is_error,
                    untrusted,
                }
            }
        }
    }
}

#[async_trait]
impl ToolDispatch for Registry {
    async fn dispatch(
        &self,
        calls: Vec<ToolInvocation>,
        events: ToolEventSink,
    ) -> Vec<ToolOutcome> {
        // Nouveau cycle de dispatch : fait décroître la fenêtre de taint.
        self.taint.begin_cycle();

        // Partition concurrent (read-only + concurrency-safe) vs série (le reste).
        // On garde l'index d'origine pour restaurer l'ordre du batch.
        let mut concurrent: Vec<(usize, ToolInvocation)> = Vec::new();
        let mut serial: Vec<(usize, ToolInvocation)> = Vec::new();
        for (i, call) in calls.into_iter().enumerate() {
            let safe = self
                .tools
                .get(&call.name)
                .is_some_and(|t| t.is_concurrency_safe() && t.is_read_only());
            if safe {
                concurrent.push((i, call));
            } else {
                serial.push((i, call));
            }
        }

        let mut indexed: Vec<(usize, ToolOutcome)> = Vec::new();

        // Batch concurrent : reads en parallèle (plafond 10). Ils peuvent marquer
        // le taint AVANT que la phase série (mutations/Bash) ne vérifie ses
        // permissions → la défense taint intra-batch est correcte.
        let concurrent_results: Vec<(usize, ToolOutcome)> = stream::iter(concurrent)
            .map(|(i, call)| {
                let events = events.clone();
                async move { (i, self.run_one(call, events).await) }
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        indexed.extend(concurrent_results);

        // Batch série : mutations une par une.
        for (i, call) in serial {
            indexed.push((i, self.run_one(call, events.clone()).await));
        }

        // Restaure l'ordre du batch (transcripts/tests déterministes).
        indexed.sort_by_key(|(i, _)| *i);
        indexed.into_iter().map(|(_, o)| o).collect()
    }
}

fn err_outcome(id: agent_core::message::ToolCallId, msg: String) -> ToolOutcome {
    // Erreur de pipeline (refus/inconnu/parse) : contenu maison, non taché.
    ToolOutcome {
        id,
        content: msg,
        is_error: true,
        untrusted: false,
    }
}

fn err_outcome_tainted(
    id: agent_core::message::ToolCallId,
    msg: String,
    untrusted: bool,
) -> ToolOutcome {
    ToolOutcome {
        id,
        content: msg,
        is_error: true,
        untrusted,
    }
}

fn ask_reason(taint_recent: bool, is_sensitive: bool) -> String {
    if taint_recent && is_sensitive {
        "action sensible issue de contenu non fiable (défense injection)".to_string()
    } else {
        "action sensible nécessitant confirmation".to_string()
    }
}

/// Résumé court d'une entrée d'outil pour le prompt de confirmation.
fn summarize(input: &serde_json::Value) -> String {
    let s = input.to_string();
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s
    }
}

/// Builder de `Registry`. L'`Approver` par défaut est `AutoDeny` (fail-closed :
/// sans interlocuteur explicite, toute confirmation échoue).
pub struct RegistryBuilder {
    tools: HashMap<String, Box<dyn DynTool>>,
    mode: PermissionMode,
    approver: Option<Arc<dyn Approver>>,
    taint_window: u64,
    ctx: ToolCtx,
}

impl RegistryBuilder {
    pub fn mode(mut self, mode: PermissionMode) -> Self {
        self.mode = mode;
        self
    }
    pub fn approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }
    pub fn taint_window(mut self, window: u64) -> Self {
        self.taint_window = window;
        self
    }
    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.ctx.timeout = timeout;
        self
    }
    /// Closure de durcissement des commandes shell (sandbox réseau Bash), injecté
    /// par l'agent-cli depuis `agent-sandbox`.
    pub fn command_hardener(mut self, harden: crate::tool::CommandHardener) -> Self {
        self.ctx.harden = Some(harden);
        self
    }
    /// Enregistre un outil natif (boxé en `DynTool`). Un nom déjà présent est
    /// remplacé.
    pub fn register<T: crate::tool::Tool + 'static>(mut self, tool: T) -> Self {
        let dyn_tool = into_dyn(tool);
        self.tools.insert(dyn_tool.name().to_string(), dyn_tool);
        self
    }
    /// Enregistre un `DynTool` déjà boxé (ex. futur outil MCP).
    pub fn register_dyn(mut self, tool: Box<dyn DynTool>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }
    pub fn build(self) -> Registry {
        Registry {
            tools: self.tools,
            mode: self.mode,
            approver: self.approver.unwrap_or_else(|| Arc::new(AutoDeny)),
            taint: TaintTracker::new(self.taint_window),
            ctx: self.ctx,
        }
    }
}
