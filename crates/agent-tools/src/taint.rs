//! Suivi du taint untrusted (ARCHITECTURE §4.6, OWASP LLM01). Toute sortie
//! d'outil est untrusted par défaut (invariant 3) ; quand l'une est produite, le
//! taint est marqué « récent » pour quelques cycles de dispatch. Une action
//! sensible déclenchée pendant cette fenêtre force la confirmation
//! (cf. `permission::resolve_permission`).
//!
//! `&self` partout (interior mutability) : le `Registry` dispatche en `&self` et
//! exécute des outils concurrents qui peuvent marquer le taint en parallèle.

use std::sync::Mutex;

#[derive(Debug)]
struct State {
    /// Cycle de dispatch courant (incrémenté à chaque batch).
    cycle: u64,
    /// Dernier cycle où du taint a été produit.
    last_marked: Option<u64>,
}

/// Fenêtre de fraîcheur par défaut (en cycles de dispatch). Le taint produit au
/// cycle N reste « récent » jusqu'au cycle N + WINDOW inclus.
pub const DEFAULT_WINDOW: u64 = 2;

#[derive(Debug)]
pub struct TaintTracker {
    window: u64,
    state: Mutex<State>,
}

impl Default for TaintTracker {
    fn default() -> Self {
        Self::new(DEFAULT_WINDOW)
    }
}

impl TaintTracker {
    pub fn new(window: u64) -> Self {
        Self {
            window,
            state: Mutex::new(State {
                cycle: 0,
                last_marked: None,
            }),
        }
    }

    /// Ouvre un nouveau cycle de dispatch (un batch d'outils). À appeler une fois
    /// au début de `dispatch`.
    pub fn begin_cycle(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.cycle = s.cycle.saturating_add(1);
        }
    }

    /// Marque du taint au cycle courant (une sortie untrusted vient d'être
    /// produite).
    pub fn mark(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.last_marked = Some(s.cycle);
        }
    }

    /// Le taint est-il récent (dans la fenêtre) ? Sert à forcer `Ask`.
    pub fn is_recent(&self) -> bool {
        match self.state.lock() {
            Ok(s) => match s.last_marked {
                Some(m) => s.cycle.saturating_sub(m) <= self.window,
                None => false,
            },
            // Mutex empoisonné : fail-closed → on considère le contexte taché.
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_tracker_has_no_taint() {
        let t = TaintTracker::default();
        t.begin_cycle();
        assert!(!t.is_recent());
    }

    #[test]
    fn mark_makes_taint_recent_in_same_cycle() {
        let t = TaintTracker::new(2);
        t.begin_cycle();
        t.mark();
        assert!(t.is_recent());
    }

    #[test]
    fn taint_decays_after_window() {
        let t = TaintTracker::new(1); // fenêtre 1
        t.begin_cycle(); // cycle 1
        t.mark(); // marqué au cycle 1
        assert!(t.is_recent());
        t.begin_cycle(); // cycle 2 : 2-1=1 ≤ 1 → encore récent
        assert!(t.is_recent());
        t.begin_cycle(); // cycle 3 : 3-1=2 > 1 → expiré
        assert!(!t.is_recent());
    }
}
