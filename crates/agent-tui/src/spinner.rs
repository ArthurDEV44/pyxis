//! Indicateurs de progression vivante (US-044/045) : spinner animé, durée écoulée,
//! estimation de tokens. Signale que la session travaille (pas gelée).
//!
//! Pur et SANS horloge : l'horloge vit dans la boucle d'`agent-cli`, qui avance
//! `AppState::spinner_tick` (~10 fps) et fournit la durée. Ce module ne fait que
//! choisir glyphe/style/verbe pour un tick donné et formater durée/tokens — donc
//! `render` reste testable via `TestBackend`.
//!
//! Dégradation reduced-motion (`NO_COLOR` / `NUMEN_REDUCED_MOTION`) : le spinner
//! animé devient un point `●` pulsé lentement (pas d'animation rapide).

use std::time::Duration;

use ratatui::style::Style;

use crate::theme::Theme;

/// Frames du spinner (rotation braille, cohérente avec l'identité braille de Numen),
/// jouées en **ping-pong** (aller-retour) plutôt qu'en boucle dure.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Verbes d'activité, rotation lente (~2 s). Champ lexical Numen : canaliser une
/// puissance brute pour la façonner en résultat utile.
const VERBS: [&str; 6] = [
    "réfléchit",
    "assemble",
    "forge",
    "canalise",
    "trame",
    "façonne",
];

/// Seuil avant d'afficher la durée (évite le clignotement sur les tours très courts,
/// US-045 AC4). Le spinner, lui, apparaît dès le début du tour.
pub(crate) const DURATION_MIN: Duration = Duration::from_secs(2);

/// Index ping-pong dans `0..n` : monte de 0 à n-1 puis redescend (aller-retour).
fn pingpong(tick: usize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let t = tick % period;
    if t < n { t } else { period - t }
}

/// Glyphe + style du spinner pour un tick donné. En reduced-motion, un `●` qui
/// pulse (accent ↔ faint) ~1 Hz, sans rotation.
pub(crate) fn frame(tick: usize, reduced_motion: bool, theme: &Theme) -> (&'static str, Style) {
    if reduced_motion {
        let bright = (tick / 10).is_multiple_of(2); // ~1 s par demi-cycle à 10 fps
        let style = if bright {
            theme.accent()
        } else {
            theme.faint()
        };
        ("●", style)
    } else {
        (FRAMES[pingpong(tick, FRAMES.len())], theme.accent())
    }
}

/// Verbe d'activité courant (rotation lente, ~1 verbe / 2 s).
pub(crate) fn verb(tick: usize) -> &'static str {
    VERBS[(tick / 20) % VERBS.len()]
}

/// Durée compacte : `Ns` sous la minute, puis `Nm Ns`.
pub(crate) fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {}s", s / 60, s % 60)
    }
}

/// Estimation de tokens compacte : `1.2k` au-delà de mille, sinon le nombre brut.
pub(crate) fn fmt_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_animates_and_pingpongs() {
        let theme = Theme::new(true);
        // Le glyphe change avec le tick (animation).
        let a = frame(0, false, &theme).0;
        let b = frame(1, false, &theme).0;
        assert_ne!(a, b, "le spinner doit s'animer");
        // Ping-pong : le sommet est atteint puis on redescend (symétrie).
        assert_eq!(pingpong(0, 4), 0);
        assert_eq!(pingpong(3, 4), 3);
        assert_eq!(pingpong(4, 4), 2);
        assert_eq!(
            pingpong(6, 4),
            0,
            "retour au point de départ après un aller-retour"
        );
    }

    #[test]
    fn reduced_motion_pulses_a_dot() {
        let theme = Theme::new(true);
        let (glyph, _) = frame(0, true, &theme);
        assert_eq!(
            glyph, "●",
            "reduced-motion : point statique, pas de rotation"
        );
        // La pulsation alterne le style sur ~1 s (10 ticks).
        let s0 = frame(0, true, &theme).1;
        let s_late = frame(10, true, &theme).1;
        assert_ne!(s0, s_late, "le point doit pulser (accent ↔ faint)");
    }

    #[test]
    fn verb_rotates_slowly() {
        assert_eq!(verb(0), verb(19), "même verbe pendant ~2 s");
        assert_ne!(verb(0), verb(20), "puis le verbe change");
    }

    #[test]
    fn duration_format_is_compact() {
        assert_eq!(fmt_duration(Duration::from_secs(4)), "4s");
        assert_eq!(fmt_duration(Duration::from_secs(75)), "1m 15s");
    }

    #[test]
    fn tokens_format_is_compact() {
        assert_eq!(fmt_tokens(842), "842");
        assert_eq!(fmt_tokens(1234), "1.2k");
        assert_eq!(fmt_tokens(84_200), "84.2k");
    }
}
