//! `agent-tokenizer` — comptage de tokens local. Headless (aucune dépendance
//! TUI/HTTP). Indispensable au fallback de `ContextBudget` quand le provider
//! n'émet pas d'`usage` en stream (cf. ARCHITECTURE §3.3 / PROVIDERS §4.3) et de
//! l'estimation pré-tour des budgets (US-014).
//!
//! Le défaut est une **heuristique** (≈ 1 token / 4 octets) : suffisant pour un
//! *seuil* de compaction (on n'a pas besoin du compte exact, juste d'un signal
//! monotone). Un compteur exact tiktoken-rs est disponible derrière le feature
//! `tiktoken`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

/// Compte des tokens à partir de texte brut. `Send + Sync` pour être injecté
/// comme `dyn TokenCounter` dans les `Deps` d'`agent-core`.
pub trait TokenCounter: Send + Sync {
    /// Estime le nombre de tokens d'un fragment de texte.
    fn count_text(&self, text: &str) -> usize;
}

/// Heuristique sans dépendance : ~1 token pour 4 octets UTF-8, plancher à 1 si
/// non vide. Volontairement conservatrice (sur-estime un peu) pour déclencher la
/// compaction *avant* la limite réelle plutôt qu'après.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicCounter;

impl TokenCounter for HeuristicCounter {
    fn count_text(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        // div_ceil : tout texte non vide vaut au moins 1 token.
        text.len().div_ceil(4)
    }
}

/// Compteur exact basé sur tiktoken-rs (BPE `cl100k_base`/`o200k_base`).
/// Disponible derrière le feature `tiktoken`. Pour les modèles non-OpenAI, c'est
/// une approximation raisonnable (meilleure que l'heuristique) du seuil.
#[cfg(feature = "tiktoken")]
pub struct TiktokenCounter {
    bpe: tiktoken_rs::CoreBPE,
}

#[cfg(feature = "tiktoken")]
impl TiktokenCounter {
    /// Construit un compteur `o200k_base` (modèles récents). Faillible : retombe
    /// sur l'heuristique en cas d'échec d'init côté appelant.
    pub fn o200k() -> Result<Self, anyhow::Error> {
        Ok(Self {
            bpe: tiktoken_rs::o200k_base()?,
        })
    }
}

#[cfg(feature = "tiktoken")]
impl TokenCounter for TiktokenCounter {
    fn count_text(&self, text: &str) -> usize {
        self.bpe.encode_ordinary(text).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_is_monotone_and_handles_empty() {
        let c = HeuristicCounter;
        assert_eq!(c.count_text(""), 0);
        assert_eq!(c.count_text("a"), 1);
        assert_eq!(c.count_text("abcd"), 1);
        assert_eq!(c.count_text("abcde"), 2);
        // monotone : plus de texte ⇒ ≥ de tokens
        assert!(c.count_text("hello world hello world") > c.count_text("hello"));
    }

    #[test]
    fn heuristic_is_object_safe() {
        let c: Box<dyn TokenCounter> = Box::new(HeuristicCounter);
        assert!(c.count_text("some text") >= 1);
    }
}
