//! Rendu Ratatui (US-019). Esthétique : **monochrome + un accent**, épurée,
//! aucune bordure lourde. Hiérarchie par poids/teinte et espace négatif, pas par
//! couleur. Signature visuelle : une gouttière `▌` qui s'allume (accent) sur le
//! tour assistant en cours de stream, et se calme (faint) une fois fini.
//!
//! `render` est PUR → testable via `TestBackend`. La dégradation sans truecolor
//! (AC4) remplace l'accent par du gras ; la mise en page est inchangée.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::state::{AppState, Block, DiffKind, PermissionPrompt, Status};

/// Palette : grayscale + un accent (teal) + un ton d'erreur (rouge muté). En
/// l'absence de truecolor, tout passe en gras/dim 16 couleurs (AC4).
pub struct Theme {
    truecolor: bool,
}

impl Theme {
    pub fn new(truecolor: bool) -> Self {
        Self { truecolor }
    }

    fn fg(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0xe4, 0xe4, 0xe4))
        } else {
            Style::default()
        }
    }
    fn dim(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a))
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }
    fn faint(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x4e, 0x4e, 0x4e))
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }
    fn accent(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x6f, 0xd0, 0xc8))
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }
    }
    fn error(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0xd0, 0x6a, 0x6a))
        } else {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
    }
}

const GUTTER: &str = "▌ ";
const INDENT: &str = "  ";

/// Rendu complet d'une frame.
pub fn render(frame: &mut Frame, state: &AppState) {
    let theme = Theme::new(state.truecolor);
    let area = frame.area();

    // En bas : soit le dialog de permission, soit (status + input).
    let bottom_height = match &state.pending {
        Some(p) => permission_height(p, area.width),
        None => 2,
    };
    let chunks =
        Layout::vertical([Constraint::Min(1), Constraint::Length(bottom_height)]).split(area);

    render_transcript(frame, chunks[0], state, &theme);

    match &state.pending {
        Some(prompt) => render_permission(frame, chunks[1], prompt, &theme),
        None => render_input(frame, chunks[1], state, &theme),
    }
}

fn render_transcript(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    for block in &state.blocks {
        push_block(&mut lines, block, theme);
        lines.push(Line::raw("")); // espace négatif entre les tours
    }

    // Auto-follow : on colle le bas (scroll 0), décalé par le scroll utilisateur.
    let total = lines.len() as u16;
    let visible = area.height;
    let max_off = total.saturating_sub(visible);
    let offset = max_off.saturating_sub(state.scroll.min(max_off));

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    frame.render_widget(para, area);
}

fn push_block(lines: &mut Vec<Line>, block: &Block, theme: &Theme) {
    match block {
        Block::User(text) => {
            lines.push(Line::from(vec![
                Span::styled("› ", theme.dim()),
                Span::styled(text.clone(), theme.fg().add_modifier(Modifier::BOLD)),
            ]));
        }
        Block::Assistant { text, streaming } => {
            // Gouttière accent si en cours de stream, fainte une fois figée.
            let gutter = if *streaming {
                theme.accent()
            } else {
                theme.faint()
            };
            for (i, raw) in text.split('\n').enumerate() {
                let g = if i == 0 { GUTTER } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(g, gutter),
                    Span::styled(raw.to_string(), theme.fg()),
                ]));
            }
        }
        Block::Reasoning(text) => {
            for raw in text.split('\n') {
                lines.push(Line::from(vec![
                    Span::styled(format!("{INDENT}· "), theme.faint()),
                    Span::styled(raw.to_string(), theme.dim().add_modifier(Modifier::ITALIC)),
                ]));
            }
        }
        Block::ToolCall { name, summary } => {
            lines.push(Line::from(vec![
                Span::styled(format!("{INDENT}⚙ "), theme.accent()),
                Span::styled(name.clone(), theme.fg()),
                Span::styled("  ", theme.dim()),
                Span::styled(truncate(summary, 100), theme.dim()),
            ]));
        }
        Block::ToolResult {
            content,
            untrusted,
            is_error,
        } => {
            let tag_style = if *is_error {
                theme.error()
            } else {
                theme.faint()
            };
            let mut head = String::from("  ");
            if *is_error {
                head.push_str("✗ ");
            } else {
                head.push_str("← ");
            }
            if *untrusted {
                head.push_str("untrusted ");
            }
            lines.push(Line::from(Span::styled(head, tag_style)));
            // corps tronqué à 6 lignes pour la respiration.
            for raw in content.split('\n').take(6) {
                lines.push(Line::from(vec![
                    Span::styled("    ", theme.faint()),
                    Span::styled(truncate(raw, 160), theme.dim()),
                ]));
            }
            if content.split('\n').count() > 6 {
                lines.push(Line::from(Span::styled("    …", theme.faint())));
            }
        }
        Block::Notice(text) => {
            lines.push(Line::from(Span::styled(
                format!("{INDENT}· {text}"),
                theme.dim(),
            )));
        }
        Block::Error(text) => {
            lines.push(Line::from(vec![
                Span::styled(format!("{INDENT}✗ "), theme.error()),
                Span::styled(text.clone(), theme.error()),
            ]));
        }
    }
}

fn render_input(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    // Ligne de statut discrète : modèle · état.
    let status_word = match state.status {
        Status::Thinking => Span::styled("● réfléchit", theme.accent()),
        Status::Idle => Span::styled("○ prêt", theme.faint()),
    };
    let status = Line::from(vec![
        Span::styled(format!("{INDENT}{}", state.model), theme.faint()),
        Span::styled("   ", theme.faint()),
        status_word,
    ]);
    frame.render_widget(Paragraph::new(status), rows[0]);

    // Ligne de saisie : prompt accent + texte + curseur.
    let input = Line::from(vec![
        Span::styled("› ", theme.accent()),
        Span::styled(state.input.clone(), theme.fg()),
        Span::styled("▏", theme.accent()),
    ]);
    frame.render_widget(Paragraph::new(input), rows[1]);
}

fn render_permission(frame: &mut Frame, area: Rect, prompt: &PermissionPrompt, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    // Titre : un accent net, sans boîte.
    lines.push(Line::from(vec![
        Span::styled("⟐ ", theme.accent()),
        Span::styled(
            prompt.title.clone(),
            theme.fg().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  — {}", prompt.reason), theme.dim()),
    ]));

    // Détail / diff : gouttière non sélectionnable (numéro de ligne faint).
    for (i, dl) in prompt.detail.iter().enumerate() {
        let (sign, sign_style, text_style) = match dl.kind {
            DiffKind::Add => ("+", theme.accent(), theme.fg()),
            DiffKind::Remove => ("-", theme.dim(), theme.dim()),
            DiffKind::Context => (" ", theme.faint(), theme.dim()),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:>3} ", i + 1), theme.faint()), // gouttière n°
            Span::styled(format!("{sign} "), sign_style),
            Span::styled(truncate(&dl.text, 140), text_style),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  [o]", theme.accent()),
        Span::styled(" autoriser   ", theme.dim()),
        Span::styled("[n]", theme.accent()),
        Span::styled(" refuser", theme.dim()),
    ]));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Hauteur nécessaire au dialog de permission (titre + détail + actions).
fn permission_height(prompt: &PermissionPrompt, _width: u16) -> u16 {
    let detail = prompt.detail.len().min(12) as u16;
    (2 + detail).clamp(2, 16)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DiffLine;
    use agent_core::AgentEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn dump(buf: &Buffer) -> String {
        let area = buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn draw(state: &AppState, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, state)).unwrap();
        dump(term.backend().buffer())
    }

    // US-019 AC1 : texte streamé rendu token-par-token, gouttière présente.
    #[test]
    fn streamed_text_renders_with_gutter() {
        let mut s = AppState::new("gpt-5", true);
        for tok in ["Bonjour ", "depuis ", "Numen"] {
            s.apply(&AgentEvent::Text(tok.into()));
        }
        let out = draw(&s, 40, 12);
        assert!(out.contains("Bonjour depuis Numen"), "{out}");
        assert!(out.contains("▌"), "gouttière accent absente:\n{out}");
        assert!(out.contains("›"), "prompt de saisie absent");
    }

    // US-019 AC2 : un diff avec gouttière (numéros) s'affiche dans le dialog.
    #[test]
    fn permission_dialog_renders_diff_gutter() {
        let mut s = AppState::new("gpt-5", true);
        s.pending = Some(PermissionPrompt {
            title: "edit src/main.rs".into(),
            reason: "mutation".into(),
            detail: vec![
                DiffLine {
                    kind: DiffKind::Remove,
                    text: "let x = 1;".into(),
                },
                DiffLine {
                    kind: DiffKind::Add,
                    text: "let x = 2;".into(),
                },
            ],
        });
        let out = draw(&s, 50, 14);
        assert!(
            out.contains("autoriser") && out.contains("refuser"),
            "{out}"
        );
        assert!(
            out.contains("- let x = 1;"),
            "ligne supprimée absente:\n{out}"
        );
        assert!(
            out.contains("+ let x = 2;"),
            "ligne ajoutée absente:\n{out}"
        );
        assert!(out.contains("edit src/main.rs"));
    }

    // US-019 AC4 : dégradation sans truecolor — pas de panic, layout intact.
    #[test]
    fn monochrome_degradation_renders_without_panic() {
        let mut s = AppState::new("gpt-5", false);
        s.apply(&AgentEvent::Text("texte mono".into()));
        let out = draw(&s, 30, 8);
        assert!(out.contains("texte mono"));
        assert!(out.contains("▌"));
    }

    // US-019 AC4 (bis) : terminal étroit → reflow sans corruption (pas de panic).
    #[test]
    fn narrow_terminal_does_not_corrupt() {
        let mut s = AppState::new("gpt-5", true);
        s.apply(&AgentEvent::Text(
            "un texte assez long pour devoir wrapper sur plusieurs lignes dans un terminal étroit"
                .into(),
        ));
        let _ = draw(&s, 16, 10);
        let _ = draw(&s, 8, 6);
        // pas de panic = indices de wrap recalculés proprement.
    }

    // Refus de permission interrompt proprement (état nettoyé) — AC3.
    #[test]
    fn refusing_permission_clears_prompt() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut s = AppState::new("gpt-5", true);
        s.pending = Some(PermissionPrompt {
            title: "bash".into(),
            reason: "sensible".into(),
            detail: vec![],
        });
        let action = s.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(action, crate::state::InputAction::Permission(false));
        assert!(s.pending.is_none());
    }
}
