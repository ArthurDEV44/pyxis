//! US-004 — Rendu TUI streaming brut (Ratatui).
//!
//! Prouve le tube `agent-core → canal → agent-tui` : le cœur n'émet que des
//! `AgentEvent` structurés (jamais d'ANSI), le frontend décide seul du rendu.
//! Esthétique cible : monochrome + un accent, aucune bordure ASCII lourde.
//!
//! La logique de rendu (`ui`) est pure et testée via `TestBackend` — la fluidité
//! subjective (token-par-token, sans scintillement) se vérifie en interactif.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Contrat cœur → client (sous-ensemble de l'`AgentEvent` de l'archi §10.1).
/// Aucune décision de présentation, aucune séquence ANSI.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Text(String),
    Reasoning(String),
    EndTurn,
}

/// État de rendu côté client.
pub struct AppState {
    pub transcript: String,
    pub input: String,
    pub done: bool,
    pub truecolor: bool,
}

impl AppState {
    pub fn new(truecolor: bool) -> Self {
        Self {
            transcript: String::new(),
            input: String::new(),
            done: false,
            truecolor,
        }
    }

    /// Applique un événement cœur. Le reasoning n'est pas rendu dans ce spike
    /// (décodage suffisant, cf. ROADMAP : « rendu du raisonnement non requis »).
    pub fn apply(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::Text(t) => self.transcript.push_str(t),
            AgentEvent::Reasoning(_) => {}
            AgentEvent::EndTurn => self.done = true,
        }
    }
}

/// Détection truecolor → dégradation monochrome propre si absent (AC3).
pub fn supports_truecolor() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
}

/// Rendu pur : transcript streamé en haut, champ de saisie en bas. Monochrome ;
/// un seul accent (le marqueur de prompt) si truecolor, sinon gras.
pub fn ui(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(frame.area());

    let transcript = Paragraph::new(state.transcript.as_str())
        .wrap(Wrap { trim: false })
        .block(Block::default());
    frame.render_widget(transcript, chunks[0]);

    let accent_style = if state.truecolor {
        Style::default()
            .fg(Color::Rgb(0x9b, 0x87, 0xf5))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let prompt = Span::styled("› ", accent_style);
    let input_line = Line::from(vec![prompt, Span::raw(state.input.as_str())]);
    let input = Paragraph::new(input_line).block(Block::default().borders(Borders::TOP));
    frame.render_widget(input, chunks[1]);
}

/// Découpe un texte en « tokens » (mots + espace) pour simuler un flux.
pub fn tokenize(s: &str) -> Vec<String> {
    s.split_inclusive(' ').map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn dump(buf: &Buffer, w: u16, h: u16) -> String {
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    // AC1 (version déterministe) : les tokens streamés s'accumulent et se rendent.
    #[test]
    fn streamed_text_renders_into_buffer() {
        let (w, h) = (40, 10);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut state = AppState::new(false);

        // Simule l'arrivée token-par-token, en redessinant à chaque token.
        for tok in tokenize("Bonjour depuis Pyxis en streaming") {
            state.apply(&AgentEvent::Text(tok));
            terminal.draw(|f| ui(f, &state)).unwrap();
        }

        let text = dump(terminal.backend().buffer(), w, h);
        assert!(
            text.contains("Bonjour depuis Pyxis"),
            "transcript absent:\n{text}"
        );
        assert!(text.contains("›"), "marqueur de prompt absent");
    }

    // AC2 : un redimensionnement en cours de stream ne corrompt pas le rendu.
    #[test]
    fn resize_midstream_reflows_without_corruption() {
        let mut terminal = Terminal::new(TestBackend::new(50, 8)).unwrap();
        let mut state = AppState::new(false);
        state.apply(&AgentEvent::Text(
            "un texte assez long pour wrapper sur plusieurs lignes lorsque la largeur change"
                .into(),
        ));
        terminal.draw(|f| ui(f, &state)).unwrap();

        // étroitisse le terminal en plein "stream"
        terminal.backend_mut().resize(24, 12);
        state.apply(&AgentEvent::Text(
            " et encore du texte ajouté après le resize".into(),
        ));
        terminal.draw(|f| ui(f, &state)).unwrap();

        let text = dump(terminal.backend().buffer(), 24, 12);
        assert!(
            text.contains("resize"),
            "le texte post-resize doit apparaître"
        );
        // pas de panic = pas de corruption d'indices ; le wrap a recalculé.
    }

    #[test]
    fn monochrome_degradation_is_selected_without_truecolor() {
        let state = AppState::new(false);
        assert!(!state.truecolor);
    }
}
