//! Palette de rendu (US-032). Esthétique **monochrome + un accent (teal)** : la
//! hiérarchie passe par le poids et la teinte, pas par la couleur. La couleur est
//! RÉSERVÉE au fonctionnel : les tons de diff (ajout/suppression) et `success`. En
//! l'absence de truecolor (AC4), tout dégrade en 16 couleurs / modifiers sans
//! perdre la distinction (la mise en page est inchangée).
//!
//! Extrait de `render.rs` pour centraliser les couleurs et garder le rendu pur.

use ratatui::style::{Color, Modifier, Style};

/// Palette : grayscale + un accent (teal) + tons fonctionnels (erreur, diff,
/// succès). `truecolor` pilote la dégradation.
pub struct Theme {
    truecolor: bool,
}

impl Theme {
    pub fn new(truecolor: bool) -> Self {
        Self { truecolor }
    }

    /// Le terminal supporte-t-il le 24 bits ? (consommé par le rendu du logo, qui
    /// interpole une teinte continue uniquement en truecolor.)
    pub fn truecolor(&self) -> bool {
        self.truecolor
    }

    // ── Chrome monochrome + accent ──────────────────────────────────────────────

    pub fn fg(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0xe4, 0xe4, 0xe4))
        } else {
            Style::default()
        }
    }
    pub fn dim(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a))
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }
    pub fn faint(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x4e, 0x4e, 0x4e))
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }
    pub fn accent(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x6f, 0xd0, 0xc8))
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }
    }
    pub fn error(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0xd0, 0x6a, 0x6a))
        } else {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
    }
    /// Fond de la ligne sélectionnée (menu de commandes) : voile teal sombre en
    /// truecolor, vidéo inverse en 16 couleurs.
    pub fn selection(&self) -> Style {
        if self.truecolor {
            Style::default().bg(Color::Rgb(0x1c, 0x2e, 0x2c))
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }
    /// Surbrillance d'un `/skill` inséré dans l'input : pastille teal sur fond sombre.
    pub fn skill_chip(&self) -> Style {
        if self.truecolor {
            Style::default()
                .fg(Color::Rgb(0x6f, 0xd0, 0xc8))
                .bg(Color::Rgb(0x1c, 0x2e, 0x2c))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    // ── Tons FONCTIONNELS (couleur autorisée car porteuse de sens) ───────────────

    /// Succès / confirmation (ex. objectif atteint).
    pub fn success(&self) -> Style {
        if self.truecolor {
            Style::default().fg(Color::Rgb(0x5a, 0xb0, 0x7a))
        } else {
            Style::default().fg(Color::Green)
        }
    }
    /// Ligne ajoutée d'un diff : fond vert sombre + texte clair (truecolor) ; en 16
    /// couleurs, vert simple (le signe `+` porte aussi le sens, pas que la couleur).
    pub fn diff_add(&self) -> Style {
        if self.truecolor {
            Style::default()
                .fg(Color::Rgb(0xb8, 0xe8, 0xc4))
                .bg(Color::Rgb(0x10, 0x2a, 0x18))
        } else {
            Style::default().fg(Color::Green)
        }
    }
    /// Ligne supprimée d'un diff : fond rouge sombre + texte clair (truecolor).
    pub fn diff_remove(&self) -> Style {
        if self.truecolor {
            Style::default()
                .fg(Color::Rgb(0xf0, 0xc0, 0xc8))
                .bg(Color::Rgb(0x2c, 0x12, 0x16))
        } else {
            Style::default().fg(Color::Red)
        }
    }
    /// Segment ajouté MOT-À-MOT (emphase intra-ligne) : fond vert saturé.
    pub fn diff_add_word(&self) -> Style {
        if self.truecolor {
            Style::default()
                .fg(Color::Rgb(0xe8, 0xff, 0xee))
                .bg(Color::Rgb(0x24, 0x5a, 0x34))
        } else {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::REVERSED)
        }
    }
    /// Segment supprimé MOT-À-MOT : fond rouge saturé.
    pub fn diff_remove_word(&self) -> Style {
        if self.truecolor {
            Style::default()
                .fg(Color::Rgb(0xff, 0xe0, 0xe6))
                .bg(Color::Rgb(0x6a, 0x24, 0x30))
        } else {
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::REVERSED)
        }
    }
}
