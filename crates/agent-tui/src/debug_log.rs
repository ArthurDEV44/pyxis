//! Trace de diagnostic du TUI, écrite dans un FICHIER : le terminal appartient au
//! rendu, un `eprintln!` y corromprait l'affichage.
//!
//! Inactive par défaut. `PYXIS_DEBUG_TUI=1` écrit dans `pyxis-tui-debug.log` sous
//! le répertoire courant (donc dans le workspace, seul emplacement inscriptible
//! quand le sandbox est actif) ; toute autre valeur est prise comme chemin.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_FILE: &str = "pyxis-tui-debug.log";

fn target() -> Option<String> {
    let value = std::env::var("PYXIS_DEBUG_TUI").ok()?;
    let value = value.trim();
    match value {
        "" | "0" | "false" => None,
        "1" | "true" => Some(DEFAULT_FILE.to_string()),
        path => Some(path.to_string()),
    }
}

/// Vrai quand la trace est active : permet à l'appelant d'éviter de composer un
/// message coûteux pour rien.
pub fn enabled() -> bool {
    target().is_some()
}

pub fn log(message: &str) {
    let Some(path) = target() else {
        return;
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{millis} {message}");
    }
}
