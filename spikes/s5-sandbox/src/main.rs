//! US-005 — Spike sandbox : Landlock FS (kernel) + proxy réseau allow-list.
//!
//! Deux preuves, deux sous-commandes (Landlock `restrict_self` est IRRÉVERSIBLE
//! pour le thread, donc on isole chaque preuve dans son propre process) :
//!   - `s5-sandbox landlock` : une écriture hors workspace est refusée au kernel.
//!   - `s5-sandbox proxy`     : un hôte hors allow-list est bloqué par le proxy.
//!
//! Landlock NE filtre PAS le réseau (cf. ADR-7 R3) : le filtrage par hostname est
//! un proxy applicatif best-effort. Ce spike tranche sa faisabilité solo.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod proxy;

fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "landlock" => run_landlock_entry(),
        "proxy" => {
            // tokio construit à la main : on garde `landlock` hors runtime
            // multi-thread (restrict_self est par-thread).
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(proxy::demo())
        }
        other => {
            eprintln!("usage: s5-sandbox <landlock|proxy>  (reçu: {other:?})");
            eprintln!("  landlock : prouve le confinement FS kernel-level");
            eprintln!("  proxy    : prouve le filtrage réseau par allow-list de hostnames");
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn run_landlock_entry() -> anyhow::Result<()> {
    sandbox_fs::run()
}

#[cfg(not(target_os = "linux"))]
fn run_landlock_entry() -> anyhow::Result<()> {
    // US-005 AC3 : dégradation explicite hors Linux.
    eprintln!(
        "[sandbox] Landlock indisponible hors Linux — sandbox FS DÉSACTIVÉ (avertissement). \
         Pyxis est Linux-first ; macOS Seatbelt / Windows en Phase 3."
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod sandbox_fs {
    use anyhow::{Result, anyhow, bail};
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus,
    };
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Applique une politique Landlock : lecture seule partout, lecture+écriture
    /// uniquement sous `workdir`. Best-effort sur l'ABI (le kernel d'Arthur > V2).
    fn enforce(workdir: &Path) -> Result<RulesetStatus> {
        let abi = ABI::V2;
        let restriction = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(abi))?
            .create()?
            // lecture seule sur toute la hiérarchie...
            .add_rule(PathBeneath::new(
                PathFd::new("/")?,
                AccessFs::from_read(abi),
            ))?
            // ...mais écriture autorisée sous le workspace uniquement.
            .add_rule(PathBeneath::new(
                PathFd::new(workdir)?,
                AccessFs::from_all(abi),
            ))?
            .restrict_self()?;
        Ok(restriction.ruleset)
    }

    pub fn run() -> Result<()> {
        let workdir: PathBuf = std::env::temp_dir().join("pyxis_spike_sandbox");
        std::fs::create_dir_all(&workdir)?;

        let status = enforce(&workdir)?;
        println!("[landlock] ruleset status = {status:?}");
        if matches!(status, RulesetStatus::NotEnforced) {
            bail!("Landlock NotEnforced — kernel sans support effectif. Verdict no-go FS.");
        }

        // 1) Écriture DANS le workspace → doit réussir.
        let inside = workdir.join("inside.txt");
        std::fs::File::create(&inside)
            .and_then(|mut f| f.write_all(b"ok"))
            .map_err(|e| anyhow!("écriture INSIDE refusée (inattendu) : {e}"))?;
        println!(
            "[landlock] write INSIDE  {} -> OK (attendu)",
            inside.display()
        );

        // 2) Écriture HORS workspace → doit être refusée au kernel (EACCES).
        let outside = Path::new("/tmp").join("pyxis_spike_OUTSIDE.txt");
        match std::fs::File::create(&outside).and_then(|mut f| f.write_all(b"escape")) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                println!(
                    "[landlock] write OUTSIDE {} -> REFUSÉ au kernel : {e} (attendu ✓)",
                    outside.display()
                );
                println!("[landlock] VERDICT: confinement FS kernel-level effectif.");
                Ok(())
            }
            Err(e) => Err(anyhow!("erreur inattendue (pas EACCES) : {e}")),
            Ok(()) => {
                let _ = std::fs::remove_file(&outside);
                Err(anyhow!(
                    "ÉCHEC SPIKE : écriture hors workspace AUTORISÉE — Landlock n'enforce pas le FS"
                ))
            }
        }
    }
}
