//! Confinement de chemins au workspace (défense applicative). Normalisation
//! LEXICALE (sans toucher le FS, donc valable même pour un fichier à créer) :
//! on résout `.`/`..` et on vérifie que le résultat reste sous la racine. Le
//! renforcement kernel anti-symlink/anti-évasion est délégué à Landlock (US-020,
//! ARCHITECTURE §4 / invariant sandbox) — ceci est la première ligne, pas la
//! seule.

use std::path::{Component, Path, PathBuf};

use crate::error::ToolError;

/// Normalise lexicalement (résout `.` et `..` sans accès disque, ne suit pas les
/// symlinks). Un `..` qui remonte au-dessus de la racine est une évasion.
fn lexical_join(base: &Path, rel: &Path) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                // Chemin absolu : on repart de zéro (sera re-vérifié contre base).
                out = PathBuf::from(comp.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    Some(out)
}

/// Résout `path` (absolu ou relatif au workspace) et vérifie le confinement.
/// Retourne le chemin normalisé absolu, ou `OutsideWorkspace`.
pub fn confine(workspace: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let requested = Path::new(path);
    let joined = if requested.is_absolute() {
        lexical_normalize(requested)
    } else {
        lexical_join(workspace, requested)
            .ok_or_else(|| ToolError::OutsideWorkspace(path.into()))?
    };
    let root = lexical_normalize(workspace);
    if joined.starts_with(&root) {
        Ok(joined)
    } else {
        Err(ToolError::OutsideWorkspace(path.into()))
    }
}

/// Normalise un chemin absolu lexicalement (résout `.`/`..`).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_stays_in_workspace() {
        let ws = Path::new("/work/repo");
        let p = confine(ws, "src/main.rs").unwrap();
        assert_eq!(p, PathBuf::from("/work/repo/src/main.rs"));
    }

    #[test]
    fn dotdot_escape_is_rejected() {
        let ws = Path::new("/work/repo");
        assert!(matches!(
            confine(ws, "../secret.txt"),
            Err(ToolError::OutsideWorkspace(_))
        ));
        assert!(matches!(
            confine(ws, "src/../../etc/passwd"),
            Err(ToolError::OutsideWorkspace(_))
        ));
    }

    #[test]
    fn absolute_path_outside_is_rejected() {
        let ws = Path::new("/work/repo");
        assert!(matches!(
            confine(ws, "/etc/passwd"),
            Err(ToolError::OutsideWorkspace(_))
        ));
    }

    #[test]
    fn absolute_path_inside_is_accepted() {
        let ws = Path::new("/work/repo");
        let p = confine(ws, "/work/repo/src/lib.rs").unwrap();
        assert_eq!(p, PathBuf::from("/work/repo/src/lib.rs"));
    }

    #[test]
    fn interior_dotdot_that_stays_inside_is_ok() {
        let ws = Path::new("/work/repo");
        let p = confine(ws, "src/foo/../bar.rs").unwrap();
        assert_eq!(p, PathBuf::from("/work/repo/src/bar.rs"));
    }
}
