//! Confinement FS kernel-level via Landlock (US-020 AC1). Politique : lecture
//! seule sur toute la hiérarchie, lecture+écriture uniquement sous le workspace.
//!
//! **Doit être appelé tôt, sur le thread principal, AVANT la construction du
//! runtime tokio** : un domaine Landlock est hérité par les threads créés
//! *après* la restriction et par les process enfants. Ainsi les workers tokio
//! ET les sous-process Bash héritent du confinement — sans le fragile `pre_exec`
//! post-fork (risque de deadlock malloc). `restrict_self` est irréversible.
//!
//! Landlock NE filtre PAS le réseau (cf. ADR-7 R3) ni les sockets D-Bus (ABI V2)
//! → le keyring (Secret Service) et le provider (HTTPS direct) restent
//! fonctionnels ; le réseau des outils est filtré séparément par le proxy.

/// Résultat de l'application du sandbox FS, à présenter à l'utilisateur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxStatus {
    /// Confinement kernel effectif (écritures hors workspace refusées au kernel).
    Enforced,
    /// Kernel sans support Landlock effectif → confinement FS **non** garanti.
    NotEnforced,
    /// Plateforme non-Linux → sandbox FS désactivé (Linux-first, AC3).
    UnsupportedPlatform,
}

impl SandboxStatus {
    /// Message d'avertissement si le confinement n'est pas effectif (`None` si OK).
    pub fn warning(&self) -> Option<&'static str> {
        match self {
            SandboxStatus::Enforced => None,
            SandboxStatus::NotEnforced => Some(
                "sandbox FS NON appliqué (kernel sans Landlock effectif) — écritures non confinées",
            ),
            SandboxStatus::UnsupportedPlatform => Some(
                "sandbox FS désactivé (hors Linux) — Pyxis est Linux-first ; écritures non confinées",
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("landlock: {0}")]
    Landlock(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Applique le confinement FS process-wide : RW sous `workspace`, read-only
/// ailleurs. À appeler sur le thread principal avant le runtime async.
#[cfg(target_os = "linux")]
pub fn enforce_process(workspace: &std::path::Path) -> Result<SandboxStatus, SandboxError> {
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus,
    };

    let abi = ABI::V2;
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| SandboxError::Landlock(e.to_string()))?
        .create()
        .map_err(|e| SandboxError::Landlock(e.to_string()))?
        // lecture seule sur toute la hiérarchie (le provider, le keyring D-Bus et
        // la résolution de chemins fonctionnent ; aucune écriture hors workspace).
        .add_rule(PathBeneath::new(
            PathFd::new("/").map_err(|e| SandboxError::Landlock(e.to_string()))?,
            AccessFs::from_read(abi),
        ))
        .map_err(|e| SandboxError::Landlock(e.to_string()))?
        // lecture + écriture uniquement sous le workspace.
        .add_rule(PathBeneath::new(
            PathFd::new(workspace).map_err(|e| SandboxError::Landlock(e.to_string()))?,
            AccessFs::from_all(abi),
        ))
        .map_err(|e| SandboxError::Landlock(e.to_string()))?
        .restrict_self()
        .map_err(|e| SandboxError::Landlock(e.to_string()))?;

    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => SandboxStatus::Enforced,
        RulesetStatus::NotEnforced => SandboxStatus::NotEnforced,
    })
}

/// Hors Linux : dégradation explicite (AC3). Le sandbox FS est désactivé ;
/// l'appelant DOIT avertir l'utilisateur via `SandboxStatus::warning`.
#[cfg(not(target_os = "linux"))]
pub fn enforce_process(_workspace: &std::path::Path) -> Result<SandboxStatus, SandboxError> {
    Ok(SandboxStatus::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_present_only_when_not_enforced() {
        assert!(SandboxStatus::Enforced.warning().is_none());
        assert!(SandboxStatus::NotEnforced.warning().is_some());
        assert!(SandboxStatus::UnsupportedPlatform.warning().is_some());
    }

    // Sur Linux avec kernel Landlock, le confinement réel est prouvé par le spike
    // s5 (process isolé : restrict_self est irréversible). Ici on vérifie juste que
    // l'appel ne panique pas et retourne un statut cohérent — SANS restreindre le
    // process de test (qui doit pouvoir continuer à écrire).
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_degrades() {
        let st = enforce_process(std::path::Path::new("/tmp")).unwrap();
        assert_eq!(st, SandboxStatus::UnsupportedPlatform);
    }
}
