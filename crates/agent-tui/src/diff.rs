//! Calcul de diff structuré pour le rendu (US-037). Dérive un diff prêt à styler
//! depuis l'`input` d'un outil mutant : `edit` (old_string → new_string, vrai diff
//! interligne + emphase intra-ligne via `similar`) ou `write` (contenu = ajouts,
//! aperçu borné). Sert AUSSI l'aperçu du dialog de permission (US-039), où bash et
//! les outils inconnus sont représentés en lignes de contexte.
//!
//! Pur et BORNÉ (jamais d'I/O, jamais de fichier relu) : le rendu (`render.rs`)
//! applique seul les couleurs. Numéros de ligne RELATIFS aux fragments d'entrée
//! (pas les numéros absolus du fichier, qu'on n'a pas sans relire le disque).

use serde_json::Value;
use similar::{ChangeTag, TextDiff};

/// Segment intra-ligne (word-diff) : un fragment de texte et s'il est mis en
/// emphase (portion réellement changée d'une ligne ajoutée/supprimée).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub emphasized: bool,
}

/// Une ligne du diff structuré.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// Ligne ajoutée (`+`), avec segments mot-à-mot.
    Add {
        lineno: Option<usize>,
        segs: Vec<Seg>,
    },
    /// Ligne supprimée (`-`), avec segments mot-à-mot.
    Remove {
        lineno: Option<usize>,
        segs: Vec<Seg>,
    },
    /// Ligne de contexte (inchangée) ou note (bash/inconnu).
    Context { lineno: Option<usize>, text: String },
    /// Séparation entre deux hunks non contigus.
    Gap,
    /// `N` lignes de plus non affichées (au-delà de la borne).
    Truncated(usize),
}

impl Row {
    /// Numéro de ligne porté par la rangée (pour calibrer la gouttière).
    pub fn lineno(&self) -> Option<usize> {
        match self {
            Row::Add { lineno, .. } | Row::Remove { lineno, .. } | Row::Context { lineno, .. } => {
                *lineno
            }
            Row::Gap | Row::Truncated(_) => None,
        }
    }
}

/// Diff structuré, prêt à styler par le rendu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub rows: Vec<Row>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Lignes de contexte sans diff (aperçu bash/inconnu pour la permission, US-039).
pub fn note<I, S>(lines: I) -> Diff
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let rows = lines
        .into_iter()
        // Assaini : l'aperçu bash/inconnu peut citer une commande/sortie adverse.
        .map(|l| Row::Context {
            lineno: None,
            text: crate::render::sanitize(&l.into()),
        })
        .collect();
    Diff { rows }
}

/// Nombre de lignes de contexte autour d'un changement (comme `diff -U3`).
const CONTEXT: usize = 3;
/// Borne dure de lignes affichées (gros edit/write) ; au-delà → `Truncated`.
const MAX_ROWS: usize = 200;
/// Garde de coût AVANT le diff (US-037 AC4) : le Myers de `similar` est O(N·D) et
/// `bound()` ne tronque que la SORTIE — au-delà de ces seuils sur l'entrée, on ne
/// diffe pas (un `old_string`/`new_string` géant fabriqué par le modèle ferait
/// exploser le coût avant toute borne).
const MAX_DIFF_LINES: usize = 4000;
const MAX_DIFF_BYTES: usize = 512 * 1024;

/// Construit le diff d'un appel d'outil mutant depuis son `input`. `None` si
/// l'outil n'est pas mutant ou si l'input est inexploitable (US-038 : pas de diff).
pub fn from_tool(name: &str, input: &Value) -> Option<Diff> {
    // L'`input` vient du modèle (adverse) et est rendu tel quel par `push_diff`
    // SANS repasser par `sanitize` (le diff ne traverse pas le chemin markdown).
    // On assainit donc ICI, choke point unique, AVANT de differ : un `new_string`
    // fabriqué portant de l'OSC/CSI ne doit jamais atteindre le terminal intact.
    use crate::render::sanitize;
    match name {
        "edit" => {
            let old_raw = input.get("old_string")?.as_str()?;
            let new_raw = input.get("new_string")?.as_str()?;
            if raw_too_large(old_raw) || raw_too_large(new_raw) {
                return Some(too_large_note(old_raw, new_raw));
            }
            let old = sanitize(old_raw);
            let new = sanitize(new_raw);
            let d = from_edit(&old, &new);
            (!d.is_empty()).then_some(d)
        }
        "write" => {
            let raw = input.get("content")?.as_str()?;
            if raw_too_large(raw) {
                return Some(note([format!(
                    "(write too large to preview: at least {} lines)",
                    bounded_line_count(raw)
                )]));
            }
            let content = sanitize(raw);
            let d = from_write(&content);
            (!d.is_empty()).then_some(d)
        }
        _ => None,
    }
}

fn raw_too_large(s: &str) -> bool {
    s.len() > MAX_DIFF_BYTES || s.lines().take(MAX_DIFF_LINES + 1).count() > MAX_DIFF_LINES
}

fn bounded_line_count(s: &str) -> usize {
    s.lines().take(MAX_DIFF_LINES + 1).count()
}

fn too_large_note(old: &str, new: &str) -> Diff {
    note([format!(
        "(diff too large to preview: at least {} -> {} lines)",
        bounded_line_count(old),
        bounded_line_count(new)
    )])
}

/// Vrai diff interligne old → new, avec emphase intra-ligne (word-diff).
fn from_edit(old: &str, new: &str) -> Diff {
    // Garde de coût (US-037 AC4) : borner AVANT de differ. Le seuil d'octets attrape
    // aussi le cas pathologique d'UNE ligne géante (où le diff intra-ligne O(L²)
    // coûterait cher sans dépasser le seuil de lignes).
    if old.len() > MAX_DIFF_BYTES
        || new.len() > MAX_DIFF_BYTES
        || old.lines().count() > MAX_DIFF_LINES
        || new.lines().count() > MAX_DIFF_LINES
    {
        return note([format!(
            "(diff too large to preview: {} -> {} lines)",
            old.lines().count(),
            new.lines().count()
        )]);
    }
    let diff = TextDiff::from_lines(old, new);
    let mut rows: Vec<Row> = Vec::new();
    for (gi, group) in diff.grouped_ops(CONTEXT).iter().enumerate() {
        if gi > 0 {
            rows.push(Row::Gap);
        }
        for op in group {
            // Numéros suivis manuellement depuis les bornes de l'op (robuste quelle
            // que soit l'API d'indices de `InlineChange`).
            let mut old_ln = op.old_range().start;
            let mut new_ln = op.new_range().start;
            for change in diff.iter_inline_changes(op) {
                let segs: Vec<Seg> = change
                    .iter_strings_lossy()
                    .map(|(emph, value)| Seg {
                        text: strip_eol(&value),
                        emphasized: emph,
                    })
                    .collect();
                match change.tag() {
                    ChangeTag::Equal => {
                        let text = segs.iter().map(|s| s.text.as_str()).collect::<String>();
                        rows.push(Row::Context {
                            lineno: Some(new_ln + 1),
                            text,
                        });
                        old_ln += 1;
                        new_ln += 1;
                    }
                    ChangeTag::Delete => {
                        rows.push(Row::Remove {
                            lineno: Some(old_ln + 1),
                            segs,
                        });
                        old_ln += 1;
                    }
                    ChangeTag::Insert => {
                        rows.push(Row::Add {
                            lineno: Some(new_ln + 1),
                            segs,
                        });
                        new_ln += 1;
                    }
                }
            }
        }
    }
    bound(rows)
}

/// Création/remplacement : tout le contenu écrit présenté en lignes ajoutées,
/// aperçu borné (on n'a pas l'ancien contenu sans relire le disque).
fn from_write(content: &str) -> Diff {
    let mut rows: Vec<Row> = Vec::new();
    let total = content.lines().count();
    for (i, line) in content.lines().enumerate() {
        if i >= MAX_ROWS {
            rows.push(Row::Truncated(total - i));
            break;
        }
        rows.push(Row::Add {
            lineno: Some(i + 1),
            segs: vec![Seg {
                text: line.to_string(),
                emphasized: false,
            }],
        });
    }
    Diff { rows }
}

/// Borne le nombre de rangées et ajoute un marqueur de troncature.
fn bound(mut rows: Vec<Row>) -> Diff {
    if rows.len() > MAX_ROWS {
        let extra = rows.len() - MAX_ROWS;
        rows.truncate(MAX_ROWS);
        rows.push(Row::Truncated(extra));
    }
    Diff { rows }
}

/// Retire le terminateur de ligne d'un segment (`similar` conserve les `\n`).
fn strip_eol(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_produces_remove_then_add_with_word_emphasis() {
        let d = from_edit("let x = 1;\n", "let x = 2;\n");
        // Une suppression + une addition (remplacement intra-ligne).
        let removes = d
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Remove { .. }))
            .count();
        let adds = d
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Add { .. }))
            .count();
        assert_eq!((removes, adds), (1, 1));
        // Au moins un segment emphasé (le `1`/`2` qui change), et pas TOUTE la ligne.
        let segs = d
            .rows
            .iter()
            .find_map(|r| match r {
                Row::Add { segs, .. } => Some(segs),
                _ => None,
            })
            .expect("expected added line");
        assert!(
            segs.iter().any(|s| s.emphasized),
            "expected inline emphasis"
        );
        assert!(
            segs.iter().any(|s| !s.emphasized),
            "inline context remains neutral"
        );
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "let x = 2;");
        assert!(!joined.contains('\n'), "line terminator stripped");
    }

    #[test]
    fn edit_keeps_context_lines_with_numbers() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nC\nd\ne\n";
        let d = from_edit(old, new);
        // La ligne 3 change ; le contexte (lignes 1,2,4,5) est conservé et numéroté.
        assert!(d.rows.iter().any(|r| matches!(
            r,
            Row::Context {
                lineno: Some(2),
                ..
            }
        )));
        assert!(d.rows.iter().any(|r| matches!(
            r,
            Row::Add {
                lineno: Some(3),
                ..
            }
        )));
    }

    #[test]
    fn no_op_edit_is_empty() {
        assert!(from_edit("same\n", "same\n").is_empty());
    }

    #[test]
    fn write_is_all_additions() {
        let d = from_write("line 1\nline 2\nline 3");
        assert_eq!(d.rows.len(), 3);
        assert!(d.rows.iter().all(|r| matches!(r, Row::Add { .. })));
        assert!(matches!(
            &d.rows[0],
            Row::Add {
                lineno: Some(1),
                ..
            }
        ));
    }

    #[test]
    fn write_bounds_huge_content() {
        let content = (0..500)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let d = from_write(&content);
        assert_eq!(d.rows.len(), MAX_ROWS + 1);
        assert!(matches!(d.rows.last(), Some(Row::Truncated(n)) if *n == 500 - MAX_ROWS));
    }

    #[test]
    fn from_tool_dispatches_and_ignores_non_mutating() {
        assert!(from_tool("edit", &json!({"old_string": "a", "new_string": "b"})).is_some());
        assert!(from_tool("write", &json!({"content": "x"})).is_some());
        assert!(from_tool("read", &json!({"path": "a.rs"})).is_none());
        // input dégénéré → None, pas de panic.
        assert!(from_tool("edit", &json!({"path": "a.rs"})).is_none());
    }

    // Sécurité : un `new_string`/`content` adverse portant de l'OSC/CSI est assaini
    // à la construction du diff (le diff ne repasse pas par `sanitize` au rendu).
    #[test]
    fn from_tool_sanitizes_adversarial_input() {
        let d = from_tool(
            "edit",
            &json!({
                "old_string": "let x = 1;",
                "new_string": "let x = 2;\x1b]0;pwned\x07\x1b[31m"
            }),
        )
        .expect("non-empty diff");
        let any_esc = d.rows.iter().any(|r| match r {
            Row::Add { segs, .. } | Row::Remove { segs, .. } => {
                segs.iter().any(|s| s.text.contains('\u{1b}'))
            }
            Row::Context { text, .. } => text.contains('\u{1b}'),
            _ => false,
        });
        assert!(!any_esc, "escape sequence not sanitized in diff");
        let w = from_tool("write", &json!({"content": "ok\x1b[2J"})).expect("diff");
        assert!(
            !matches!(&w.rows[0], Row::Add { segs, .. } if segs[0].text.contains('\u{1b}')),
            "write not sanitized"
        );
    }

    // US-037 AC4 : la garde de coût borne `from_edit` sur une entrée géante (le diff
    // Myers ne tourne pas) → repli `note` borné, sans panic ni explosion de coût.
    #[test]
    fn from_edit_bounds_huge_input() {
        let huge = "x\n".repeat(MAX_DIFF_LINES + 100);
        let d = from_tool("edit", &json!({"old_string": "a", "new_string": huge})).expect("diff");
        assert!(
            d.rows.len() <= 2 && d.rows.iter().all(|r| matches!(r, Row::Context { .. })),
            "bounded fallback expected, not a full diff"
        );
        // Idem sur une ligne unique géante (seuil d'octets).
        let big_line = "y".repeat(MAX_DIFF_BYTES + 1);
        let d2 = from_tool("edit", &json!({"old_string": "a", "new_string": big_line})).expect("d");
        assert!(matches!(&d2.rows[0], Row::Context { .. }));
    }

    #[test]
    fn note_builds_context_rows() {
        let d = note(["rm -rf /tmp/x".to_string()]);
        assert_eq!(d.rows.len(), 1);
        assert!(
            matches!(&d.rows[0], Row::Context { lineno: None, text } if text == "rm -rf /tmp/x")
        );
    }
}
