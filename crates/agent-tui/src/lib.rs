//! `agent-tui` — frontend terminal de Pyxis (US-019). CLIENT du cœur headless :
//! il consomme les `agent_core::AgentEvent` (jamais d'ANSI venant du cœur) et
//! décide seul du rendu. Esthétique monochrome + un accent, épurée (Rauch/Vercel)
//! — la signature est une gouttière `▌` qui s'allume sur le tour en cours.
//!
//! Découpage : `state` (transcript + clavier, pur, testable), `theme` (palette
//! monochrome + accent, pure), `render` (Ratatui pur, `TestBackend`), `markdown`
//! (réponses markdown → spans), `tool` (view-models d'outils → labels/résumés),
//! `term` (raw mode, alt screen). La boucle d'orchestration (crossterm ↔ stream
//! d'agent ↔ permissions) vit dans `agent-cli`, qui assemble ces briques.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cache;
pub mod diff;
mod highlight;
mod markdown;
mod measure;
pub mod render;
mod spinner;
pub mod state;
pub mod term;
pub mod theme;
mod tool;

pub use render::render;
pub use state::{
    AppState, Block, COMMANDS, InputAction, McpServerMeta, McpStatus, MenuItem, PermissionPrompt,
    SessionMeta, Status, blocks_from_messages, prompts_from_messages,
};
pub use term::{Tui, enter, leave, supports_truecolor};
pub use theme::Theme;
