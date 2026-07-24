//! Catalogue de modèles **découvert à chaud** sur le backend ChatGPT/Codex
//! (`GET /models`). Le backend renvoie exactement les modèles accessibles au
//! compte connecté (il applique lui-même `available_in_plans`) et filtre sur le
//! `client_version` annoncé (cf. `agent_auth::oauth::openai_chatgpt::CODEX_CLIENT_VERSION`).
//!
//! Remplace une table de slugs figée dans le binaire : la liste blanche du backend
//! bouge (retraits/ajouts fréquents), donc la seule source correcte est le backend.

use serde::Deserialize;

/// Modèle présentable à l'utilisateur, réduit aux champs dont le client a besoin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub slug: String,
    pub display_name: String,
    /// Effort de raisonnement appliqué à défaut de choix explicite.
    pub default_reasoning_effort: Option<String>,
    /// Efforts acceptés par ce modèle (`low`…`ultra` selon le modèle).
    pub supported_reasoning_efforts: Vec<String>,
}

#[derive(Deserialize)]
struct WireCatalog {
    #[serde(default)]
    models: Vec<WireModel>,
}

#[derive(Deserialize)]
struct WireModel {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    /// `list` (visible dans le sélecteur), `hide` ou `none` (usage interne, ex.
    /// `codex-auto-review`).
    #[serde(default)]
    visibility: Option<String>,
    /// Ordre d'affichage voulu par le backend (croissant, 1 = tête de liste).
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<WireReasoningLevel>,
}

#[derive(Deserialize)]
struct WireReasoningLevel {
    effort: String,
}

/// Parse la réponse `/models` : ne garde que les modèles sélectionnables et
/// respecte l'ordre `priority` du backend. Tolérant aux champs inconnus (le
/// backend en ajoute régulièrement).
pub fn parse_catalog(body: &str) -> Result<Vec<CatalogModel>, serde_json::Error> {
    let mut wire: Vec<WireModel> = serde_json::from_str::<WireCatalog>(body)?
        .models
        .into_iter()
        .filter(|m| matches!(m.visibility.as_deref(), None | Some("list")))
        .collect();
    wire.sort_by_key(|m| m.priority);
    Ok(wire
        .into_iter()
        .map(|m| CatalogModel {
            display_name: m.display_name.unwrap_or_else(|| m.slug.clone()),
            slug: m.slug,
            default_reasoning_effort: m.default_reasoning_level,
            supported_reasoning_efforts: m
                .supported_reasoning_levels
                .into_iter()
                .map(|level| level.effort)
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extrait réel de la réponse backend (2026-07-24), champs inconnus inclus.
    const SAMPLE: &str = r#"{
      "models": [
        {"slug":"gpt-5.4","display_name":"GPT-5.4","visibility":"list","priority":16,
         "default_reasoning_level":"medium","context_window":272000,
         "supported_reasoning_levels":[{"effort":"low","description":"x"},{"effort":"high","description":"y"}]},
        {"slug":"codex-auto-review","display_name":"Codex Auto Review","visibility":"hide","priority":43,
         "default_reasoning_level":"medium","supported_reasoning_levels":[{"effort":"medium"}]},
        {"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","visibility":"list","priority":1,
         "default_reasoning_level":"low",
         "supported_reasoning_levels":[{"effort":"low"},{"effort":"max"},{"effort":"ultra"}]}
      ]
    }"#;

    #[test]
    fn keeps_listed_models_ordered_by_priority() {
        let catalog = parse_catalog(SAMPLE).expect("sample catalog parses");
        let slugs: Vec<&str> = catalog.iter().map(|m| m.slug.as_str()).collect();
        assert_eq!(slugs, ["gpt-5.6-sol", "gpt-5.4"], "hidden model dropped");
        assert_eq!(catalog[0].default_reasoning_effort.as_deref(), Some("low"));
        assert_eq!(
            catalog[0].supported_reasoning_efforts,
            ["low", "max", "ultra"]
        );
        assert_eq!(catalog[1].display_name, "GPT-5.4");
    }

    #[test]
    fn empty_catalog_is_not_an_error() {
        assert!(
            parse_catalog(r#"{"models":[]}"#)
                .expect("empty catalog parses")
                .is_empty()
        );
    }
}
