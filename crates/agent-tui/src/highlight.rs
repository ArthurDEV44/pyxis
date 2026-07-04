//! Coloration syntaxique des code-blocks et des diffs (spike US-040 + US-042).
//!
//! ## Choix du moteur (décision du spike US-040)
//! `syntect` 5.3 en **`default-fancy`** (regex `fancy-regex`, **pur Rust** : aucune
//! toolchain C, contrairement au défaut oniguruma) + **`two-face`** 0.5 pour les
//! grammaires. Rationale :
//! - **Pas de dépendance C** → binaire distribuable sans toolchain (contrainte dure
//!   du PRD) ; `synoptic` est aussi C-free mais ses règles regex sont plus
//!   grossières que les grammaires Sublime.
//! - **Qualité** : grammaires Sublime fidèles. Le jeu PAR DÉFAUT de syntect ne
//!   couvre PAS TypeScript ni TOML (vérifié sur le packdump) ; `two-face` embarque
//!   le set curé de `bat` qui couvre Rust/TS-JS/JSON/TOML/Markdown (les 5 langages
//!   exigés). `two-face` n'est que des dumps embarqués (pur Rust, syntect 5.3).
//! - **Éprouvé** : exactement la stack du Codex CLI (Rust + ratatui).
//! - **Coût** : ~3 Mo de binaire, acceptable. Écarté : `synoptic` (grammaires plus
//!   pauvres), `syntect`+`onig` (toolchain C).
//!
//! ## Performance (cf. US-041)
//! La coloration syntect est STATEFUL ligne à ligne et coûteuse → elle ne tourne
//! JAMAIS par frame : `render.rs` mémoïse les lignes déjà colorées (cache par bloc).
//! Le `SyntaxSet` et le `Theme` ne sont chargés QU'UNE fois (`OnceLock` global).
//!
//! ## Dégradation
//! La coloration ne s'applique QU'EN truecolor : syntect n'embarque pas de thème
//! ANSI 16-couleurs et hardcoder du RGB hors truecolor corromprait la palette
//! (leçon Codex). Sans truecolor → `None` → l'appelant retombe sur le rendu
//! monochrome (dim) existant. Langage non couvert → `None` (texte neutre).

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme::Theme as UiTheme;

/// Borne de longueur (octets) au-delà de laquelle une ligne n'est PAS colorée :
/// évite de lancer le moteur regex sur une ligne minifiée géante (coût linéaire par
/// règle de grammaire). Généreux : les vraies lignes de code sont bien en deçà.
const MAX_HL_BYTES: usize = 16 * 1024;
const MAX_CODE_BLOCK_BYTES: usize = 128 * 1024;
const MAX_CODE_BLOCK_LINES: usize = 2_000;

/// Grammaires (two-face : Rust/TS-JS/JSON/TOML/Markdown…), chargées une seule fois.
fn syntaxes() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(two_face::syntax::extra_newlines)
}

/// Thème de coloration : `base16-ocean.dark`, sobre et sombre — la couleur du code
/// est FONCTIONNELLE (lisibilité), pas décorative. Chargé une fois depuis les thèmes
/// embarqués de syntect ; `None` si absent (dégrade en non-coloré, jamais de panic).
fn theme() -> Option<&'static Theme> {
    static TH: OnceLock<Option<Theme>> = OnceLock::new();
    TH.get_or_init(|| ThemeSet::load_defaults().themes.remove("base16-ocean.dark"))
        .as_ref()
}

/// Résout un libellé de langage (ou une extension) vers une grammaire. Normalise les
/// alias courants vers l'extension canonique (fiable via `find_syntax_by_extension`),
/// avec repli sur le token. `None` si non couvert → pas de coloration.
fn syntax_for(ss: &'static SyntaxSet, lang: &str) -> Option<&'static SyntaxReference> {
    let l = lang.trim().to_ascii_lowercase();
    if l.is_empty() {
        return None;
    }
    let ext = match l.as_str() {
        "rust" => "rs",
        "typescript" => "ts",
        "javascript" | "mjs" | "cjs" => "js",
        "markdown" => "md",
        "shell" | "bash" | "zsh" | "sh" => "sh",
        "yml" => "yaml",
        other => other,
    };
    ss.find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_token(&l))
}

/// Couleur ratatui depuis une couleur syntect (RGB ; alpha et fond ignorés — on
/// garde notre propre fond, et on ne prend QUE la teinte de premier plan).
fn to_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

/// Colore un code-block MULTI-LIGNES : rendu stateful ligne à ligne (le contexte des
/// chaînes/commentaires multi-lignes est préservé). Renvoie les spans colorés par
/// ligne (sans indentation : l'appelant pose la gouttière). `None` si non-truecolor
/// ou langage non couvert → l'appelant retombe sur le rendu neutre (dim).
pub fn code_block(code: &str, lang: &str, ui: &UiTheme) -> Option<Vec<Vec<Span<'static>>>> {
    if !ui.truecolor() {
        return None;
    }
    if code.len() > MAX_CODE_BLOCK_BYTES
        || code.lines().take(MAX_CODE_BLOCK_LINES + 1).count() > MAX_CODE_BLOCK_LINES
        || code.lines().any(|line| line.len() > MAX_HL_BYTES)
    {
        return None;
    }
    let ss = syntaxes();
    let syntax = syntax_for(ss, lang)?;
    let theme = theme()?;
    let mut h = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        // Une erreur de coloration (rare) ne doit pas casser le rendu : on stoppe et
        // l'appelant retombe sur le neutre pour le reste.
        let ranges = h.highlight_line(line, ss).ok()?;
        out.push(spans_from_ranges(&ranges));
    }
    Some(out)
}

/// Couleur de syntaxe par CARACTÈRE pour une ligne isolée (état réinitialisé :
/// best-effort sur les constructions multi-lignes, suffisant pour une ligne de hunk).
/// Sert à teinter le contenu d'un diff sans toucher au fond ajout/suppression
/// (US-042). `None` si non-truecolor ou langage non couvert.
pub fn line_colors(line: &str, lang: &str, ui: &UiTheme) -> Option<Vec<Color>> {
    if !ui.truecolor() {
        return None;
    }
    // Borne de coût : une ligne géante (fichier minifié, diff d'un `write`) passerait
    // toutes les règles regex de la grammaire sur tout son contenu. Au-delà du seuil,
    // pas de coloration (le diff reste lisible, fond +/- conservé). Seuls les premiers
    // `width` chars sont affichés de toute façon.
    if line.len() > MAX_HL_BYTES {
        return None;
    }
    let ss = syntaxes();
    let syntax = syntax_for(ss, lang)?;
    let theme = theme()?;
    let mut h = HighlightLines::new(syntax, theme);
    let with_nl = format!("{line}\n");
    let ranges = h.highlight_line(&with_nl, ss).ok()?;
    let mut cols = Vec::new();
    for (st, text) in ranges {
        let color = to_color(st.foreground);
        for _ in text.trim_end_matches(['\n', '\r']).chars() {
            cols.push(color);
        }
    }
    Some(cols)
}

/// Convertit les ranges syntect d'une ligne en spans ratatui (teinte fg seule, fond
/// par défaut). Le terminateur de ligne est retiré ; les segments vides sont écartés.
fn spans_from_ranges(ranges: &[(syntect::highlighting::Style, &str)]) -> Vec<Span<'static>> {
    ranges
        .iter()
        .filter_map(|(st, text)| {
            let t = text.trim_end_matches(['\n', '\r']);
            (!t.is_empty())
                .then(|| Span::styled(t.to_string(), Style::default().fg(to_color(st.foreground))))
        })
        .collect()
}

/// Devine le langage d'un fichier depuis l'extension de son chemin (pour colorer un
/// diff). `None` si pas d'extension exploitable.
pub fn lang_from_path(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme as UiTheme;

    #[test]
    fn no_color_without_truecolor() {
        let ui = UiTheme::new(false);
        assert!(code_block("let x = 1;", "rust", &ui).is_none());
        assert!(line_colors("let x = 1;", "rust", &ui).is_none());
    }

    #[test]
    fn required_languages_resolve() {
        // Les 5 langages exigés par US-042 doivent avoir une grammaire (two-face).
        let ss = syntaxes();
        for lang in ["rust", "ts", "js", "json", "toml", "md"] {
            assert!(
                syntax_for(ss, lang).is_some(),
                "grammaire manquante pour {lang}"
            );
        }
    }

    #[test]
    fn unknown_language_falls_back_to_none() {
        let ui = UiTheme::new(true);
        assert!(
            code_block("???", "langage-bidon-inconnu", &ui).is_none(),
            "langage inconnu → pas de coloration (texte neutre)"
        );
    }

    #[test]
    fn code_block_colors_in_truecolor() {
        let ui = UiTheme::new(true);
        let lines = code_block("let x = 1;\nlet y = 2;\n", "rust", &ui)
            .expect("rust devrait être coloré en truecolor");
        assert_eq!(lines.len(), 2);
        // Au moins un span coloré (RGB) sur la première ligne.
        assert!(
            lines[0]
                .iter()
                .any(|s| matches!(s.style.fg, Some(Color::Rgb(..))))
        );
    }

    #[test]
    fn line_colors_match_char_count() {
        let ui = UiTheme::new(true);
        let line = "let x = 1;";
        let cols = line_colors(line, "rust", &ui).expect("expected highlighting");
        assert_eq!(
            cols.len(),
            line.chars().count(),
            "one color per character for diff alignment"
        );
    }

    #[test]
    fn line_colors_handles_multibyte() {
        // Contrat critique pour l'overlay du diff : UNE couleur par `char`, même sur
        // des caractères multi-octets (sinon la teinte se désaligne en silence).
        let ui = UiTheme::new(true);
        let line = "let tea = 1; // ☕";
        let cols = line_colors(line, "rust", &ui).expect("expected highlighting");
        assert_eq!(
            cols.len(),
            line.chars().count(),
            "one color per char, including multibyte"
        );
    }

    #[test]
    fn line_colors_skips_giant_line() {
        // Borne de coût : une ligne au-delà du seuil n'est pas colorée (pas de regex
        // sur un contenu minifié géant) → repli neutre côté appelant.
        let ui = UiTheme::new(true);
        let giant = "a".repeat(MAX_HL_BYTES + 1);
        assert!(
            line_colors(&giant, "rust", &ui).is_none(),
            "giant line should not be highlighted"
        );
    }

    #[test]
    fn code_block_skips_giant_input() {
        let ui = UiTheme::new(true);
        let giant_block = "a\n".repeat(MAX_CODE_BLOCK_LINES + 1);
        assert!(code_block(&giant_block, "rust", &ui).is_none());

        let giant_line = "a".repeat(MAX_HL_BYTES + 1);
        assert!(code_block(&giant_line, "rust", &ui).is_none());
    }

    #[test]
    fn lang_from_path_reads_extension() {
        assert_eq!(lang_from_path("src/main.rs").as_deref(), Some("rs"));
        assert_eq!(lang_from_path("a/b/c.toml").as_deref(), Some("toml"));
        assert_eq!(lang_from_path("Makefile"), None);
        assert_eq!(lang_from_path(".gitignore"), None);
    }
}
