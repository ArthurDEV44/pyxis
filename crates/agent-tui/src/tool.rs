//! View-models de rendu des outils (US-035). Transforme un appel d'outil (nom +
//! `input` JSON) et son résultat en libellé `Verb(cible)` et en résumé secondaire
//! (`⎿`). C'est l'équivalent Rust des `renderToolUseMessage` /
//! `renderToolResultMessage` de Claude Code : `render.rs` ne connaît pas les
//! outils, il délègue ici. Pur et testable ; aucune décision de couleur de chrome
//! (seuls les nombres sont mis en évidence).
//!
//! Les résumés sont dérivés de l'`input` du call et du `content` du résultat :
//! aucun changement des contrats `agent-core` / `agent-tools` (US-033).

use ratatui::style::Modifier;
use ratatui::text::Span;
use serde_json::Value;

use crate::measure;
use crate::render::sanitize;
use crate::theme::Theme;

/// Libellé d'un appel d'outil : un verbe d'action + une cible optionnelle, rendus
/// `Verb(cible)`. Verbe en clair (anglais, comme le harness de référence).
pub struct Label {
    pub verb: String,
    pub target: Option<String>,
}

/// Verbe + cible affichés pour un appel d'outil, dérivés du nom et de l'input. Un
/// outil inconnu retombe sur son nom brut comme verbe + une cible best-effort
/// (US-035 : jamais de panic sur un outil non reconnu).
pub fn label(name: &str, input: &Value) -> Label {
    let verb = match name {
        "read" => "Read",
        "write" => "Write",
        "edit" => "Update",
        "glob" => "List",
        "grep" => "Search",
        "bash" => "Run",
        other => other,
    }
    .to_string();

    let target = match name {
        "bash" => str_field(input, "command").map(|s| first_line_trunc(&s, 64)),
        "grep" => str_field(input, "pattern").map(|s| trunc(&s, 56)),
        _ => str_field(input, "path")
            .or_else(|| str_field(input, "pattern").map(|s| trunc(&s, 56)))
            .or_else(|| str_field(input, "command").map(|s| first_line_trunc(&s, 64))),
    };

    Label { verb, target }
}

/// Résumé secondaire (`⎿`) d'un résultat d'outil RÉUSSI, en spans (les nombres
/// sont mis en évidence). `call` = `(name, input)` apparié par id, ou `None` si le
/// résultat est orphelin (US-033 : affichage générique dégradé, sans panic).
pub fn result_summary(
    call: Option<(&str, &Value)>,
    content: &str,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let dim = theme.dim();
    let num = theme.fg().add_modifier(Modifier::BOLD);

    let Some((name, input)) = call else {
        return vec![Span::styled(first_line_trunc(&sanitize(content), 80), dim)];
    };

    match name {
        // Lignes lues = lignes numérotées (format `{lineno}\t{ligne}` de l'outil read).
        "read" => count(
            "Read ",
            content.lines().filter(|l| l.contains('\t')).count(),
            "line",
            "lines",
            dim,
            num,
        ),
        "write" => count(
            "Wrote ",
            line_count(&str_field(input, "content").unwrap_or_default()),
            "line",
            "lines",
            dim,
            num,
        ),
        // Approximation EP-010 (sans lib de diff) : lignes du nouveau vs ancien
        // texte. Le compte exact (diff réel) arrive en EP-011 (US-038).
        "edit" => {
            let added = line_count(&str_field(input, "new_string").unwrap_or_default());
            let removed = line_count(&str_field(input, "old_string").unwrap_or_default());
            let mut s = vec![
                Span::styled("Added ", dim),
                Span::styled(added.to_string(), num),
            ];
            s.push(Span::styled(unit(added, " line", " lines"), dim));
            s.push(Span::styled(", removed ", dim));
            s.push(Span::styled(removed.to_string(), num));
            s.push(Span::styled(unit(removed, " line", " lines"), dim));
            s
        }
        "glob" => count("Found ", listed(content), "file", "files", dim, num),
        "grep" => count("Found ", listed(content), "match", "matches", dim, num),
        "bash" => {
            let n = content.lines().filter(|l| !l.trim().is_empty()).count();
            if n == 0 {
                vec![Span::styled("Ran (no output)", dim)]
            } else {
                count("Ran · ", n, "line", "lines", dim, num)
            }
        }
        _ => vec![Span::styled(first_line_trunc(&sanitize(content), 80), dim)],
    }
}

/// Message d'erreur d'un résultat d'outil, en une ligne préfixée `Error:`, ANSI
/// strippé (US-036 : pas de résidu ANSI venant d'une sortie d'outil colorée).
pub fn error_summary(content: &str) -> String {
    let clean = sanitize(content);
    let first = clean
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(failure without message)");
    let first = trunc(first, 120);
    if first.starts_with("Error") || first.starts_with("error") {
        first
    } else {
        format!("Error: {first}")
    }
}

/// Nombre de lignes non vides AU-DELÀ de la première (indicateur `… +N lignes`
/// quand une erreur multi-lignes est bornée à sa 1re ligne, US-036).
pub fn extra_lines(content: &str) -> usize {
    sanitize(content)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .count()
        .saturating_sub(1)
}

/// Le contenu d'un résultat `is_error` correspond-il à un REJET de permission
/// (refus utilisateur ou refus par le mode) plutôt qu'à une vraie erreur d'outil ?
/// La distinction pilote la teinte : atténué (rejet volontaire) vs rouge (US-036).
/// ANCRÉ sur les deux messages du Registry (`registry.rs` : « permission refusée
/// pour … » / « action « … » refusée par l'utilisateur ») plutôt qu'un substring
/// flottant « refusée » : une vraie erreur d'outil qui cite ce mot (ex. sortie bash
/// « connexion refusée ») ne doit pas être prise pour un refus volontaire.
pub fn is_user_rejection(content: &str) -> bool {
    let t = content.trim_start();
    t.starts_with("permission denied for") || t.starts_with("action \"")
}

/// Libellé d'un rejet (ton atténué) : 1re ligne non vide, ANSI strippé.
pub fn reject_summary(content: &str) -> String {
    let clean = sanitize(content);
    let first = clean
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("action rejected");
    trunc(first, 120)
}

// ── Helpers purs ────────────────────────────────────────────────────────────────

fn str_field(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Nombre de lignes affichables d'un listing (exclut le pied de troncature `…`).
fn listed(content: &str) -> usize {
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('…'))
        .count()
}

/// Nombre de lignes d'un texte (`""` → 0).
fn line_count(s: &str) -> usize {
    if s.is_empty() { 0 } else { s.lines().count() }
}

fn unit(n: usize, singular: &str, plural: &str) -> String {
    if n == 1 { singular } else { plural }.to_string()
}

/// `{prefix}{n} {unité}` avec le nombre en évidence.
fn count(
    prefix: &str,
    n: usize,
    singular: &str,
    plural: &str,
    dim: ratatui::style::Style,
    num: ratatui::style::Style,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(prefix.to_string(), dim),
        Span::styled(n.to_string(), num),
        Span::styled(format!(" {}", if n == 1 { singular } else { plural }), dim),
    ]
}

/// Tronque à `max` colonnes (char-aware, ellipse `…`).
fn trunc(s: &str, max: usize) -> String {
    measure::truncate(s, max)
}

/// 1re ligne non vide, tronquée (pour bash/commande multi-lignes).
fn first_line_trunc(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    trunc(line, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn label_maps_known_verbs_and_target() {
        let l = label(
            "edit",
            &json!({"path": "src/main.rs", "old_string": "a", "new_string": "b"}),
        );
        assert_eq!(l.verb, "Update");
        assert_eq!(l.target.as_deref(), Some("src/main.rs"));
        let b = label("bash", &json!({"command": "cargo test\nsecond line"}));
        assert_eq!(b.verb, "Run");
        assert_eq!(b.target.as_deref(), Some("cargo test"));
    }

    #[test]
    fn label_unknown_tool_falls_back_to_name() {
        let l = label("mcp__srv__do", &json!({"x": 1}));
        assert_eq!(l.verb, "mcp__srv__do");
        assert!(l.target.is_none(), "pas de panic, cible best-effort vide");
    }

    #[test]
    fn edit_summary_counts_added_and_removed() {
        let theme = Theme::new(false);
        let spans = result_summary(
            Some((
                "edit",
                &json!({"old_string": "x\ny", "new_string": "a\nb\nc"}),
            )),
            "Edited: f (level 1)",
            &theme,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Added 3 lines, removed 2 lines");
    }

    #[test]
    fn write_summary_singular_plural() {
        let theme = Theme::new(false);
        let one = result_summary(Some(("write", &json!({"content": "seule"}))), "", &theme);
        assert_eq!(
            one.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "Wrote 1 line"
        );
    }

    #[test]
    fn read_summary_counts_numbered_lines() {
        let theme = Theme::new(false);
        let content = "     1\tfn main() {\n     2\t}\n(fin)";
        let spans = result_summary(Some(("read", &json!({"path": "a.rs"}))), content, &theme);
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "Read 2 lines"
        );
    }

    #[test]
    fn orphan_result_degrades_without_panic() {
        let theme = Theme::new(false);
        let spans = result_summary(None, "some output\nnext", &theme);
        assert_eq!(
            spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "some output"
        );
    }

    #[test]
    fn error_summary_prefixes_once() {
        assert_eq!(error_summary("anchor not found"), "Error: anchor not found");
        assert_eq!(
            error_summary("Error: already prefixed"),
            "Error: already prefixed"
        );
        assert_eq!(error_summary("   \n  real message"), "Error: real message");
    }

    #[test]
    fn error_summary_strips_ansi() {
        let out = error_summary("\u{1b}[31mRed error\u{1b}[0m");
        assert_eq!(out, "Error: Red error");
        assert!(!out.contains('\u{1b}'), "ANSI residue: {out:?}");
    }

    #[test]
    fn extra_lines_counts_beyond_first() {
        assert_eq!(extra_lines("single"), 0);
        assert_eq!(extra_lines("a\nb\nc"), 2);
        assert_eq!(extra_lines("a\n\n  \nb"), 1);
    }

    #[test]
    fn rejection_detected_from_registry_messages() {
        assert!(is_user_rejection("action \"edit\" rejected by user"));
        assert!(is_user_rejection(
            "permission denied for \"bash\" (mode Plan)"
        ));
        assert!(!is_user_rejection("anchor not found"));
        assert!(!is_user_rejection("curl: (7) connection refused by host"));
        assert!(!is_user_rejection(
            "the connection was refused by the server"
        ));
    }
}
