//! `numen` — binaire CLI. SEUL crate qui câble tout (ARCHITECTURE §2) : cœur +
//! provider abonnement ChatGPT + outils + session + sandbox + frontend TUI.
//!
//! Ordre critique : le **sandbox FS (Landlock) est appliqué sur le thread
//! principal AVANT la construction du runtime tokio** → les workers et les
//! sous-process Bash héritent du confinement (fork-safe, cf. `agent_sandbox::fs`).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod approver;
mod interactive;
mod session;

use std::sync::Arc;

use agent_auth::store;
use agent_core::clock::SystemClock;
use agent_core::message::Message;
use agent_core::provider::Provider;
use agent_core::{AgentContext, Deps, RunConfig};
use agent_provider::{KEYRING_ACCOUNT, OpenAiChatGptProvider};
use agent_sandbox::{ProxyPolicy, set_proxy_env};
use agent_tokenizer::HeuristicCounter;
use agent_tools::permission::{AutoApprove, AutoDeny, PermissionMode};
use agent_tools::{Bash, Edit, Glob, Grep, Read, Registry, Write};

use crate::approver::TuiApprover;
use crate::interactive::InteractiveConfig;
use crate::session::SharedSession;

const SYSTEM_PROMPT: &str = "Tu es Numen, un agent de codage en terminal. Tu travailles dans le \
    workspace courant. Utilise les outils (read, glob, grep, write, edit, bash) pour explorer et \
    modifier le code. Sois concis et direct ; confine tes actions au workspace.";

struct Args {
    prompt: Option<String>,
    model: String,
    allow_hosts: Vec<String>,
    yes: bool,
    sandbox: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        prompt: None,
        model: "gpt-5".to_string(),
        allow_hosts: Vec::new(),
        yes: false,
        sandbox: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--print" => args.prompt = it.next(),
            "--model" => {
                if let Some(m) = it.next() {
                    args.model = m;
                }
            }
            "--allow" => {
                if let Some(h) = it.next() {
                    args.allow_hosts.push(h);
                }
            }
            "--yes" | "-y" => args.yes = true,
            "--no-sandbox" => args.sandbox = false,
            other => {
                // un argument nu sans -p est traité comme le prompt (mode -p implicite).
                if args.prompt.is_none() && !other.starts_with('-') {
                    args.prompt = Some(other.to_string());
                }
            }
        }
    }
    args
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    let workspace = std::env::current_dir()?;

    // Sandbox FS AVANT le runtime (thread principal → hérité par les workers).
    if args.sandbox {
        match agent_sandbox::enforce_process(&workspace) {
            Ok(status) => {
                if let Some(w) = status.warning() {
                    eprintln!("[sandbox] {w}");
                }
            }
            Err(e) => eprintln!("[sandbox] échec d'application : {e} — écritures non confinées"),
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(args, workspace))
}

async fn run(args: Args, workspace: std::path::PathBuf) -> anyhow::Result<()> {
    // 1. Credential abonnement ChatGPT (keyring). Absente → on guide vers le login.
    let cred = match store::load(KEYRING_ACCOUNT)? {
        Some(agent_auth::Credential::Oauth(o)) => o,
        _ => {
            anyhow::bail!(
                "Pas de credential ChatGPT. Connecte-toi d'abord :\n  \
                 cargo run -p agent-auth --example login"
            );
        }
    };
    let provider: Arc<dyn Provider> = Arc::new(OpenAiChatGptProvider::new(
        cred,
        agent_provider::DEFAULT_MAX_CONTEXT,
        Some(agent_provider::DEFAULT_REASONING_EFFORT.to_string()),
    ));

    // 2. Proxy réseau allow-list (fail-closed). Durcit les commandes Bash.
    let proxy = agent_sandbox::spawn_proxy(ProxyPolicy::new(args.allow_hosts.clone())).await?;
    let proxy_addr = proxy.addr.clone();
    let harden: agent_tools::CommandHardener =
        Arc::new(move |cmd: &mut tokio::process::Command| set_proxy_env(cmd, &proxy_addr));

    // 3. Session persistante (JSONL sous <workspace>/.numen) + snapshot mémoire.
    let session_dir = workspace.join(".numen").join("sessions");
    std::fs::create_dir_all(&session_dir)?;
    let jsonl = agent_session::JsonlSession::create_in(&session_dir)
        .map_err(|e| anyhow::anyhow!("session : {e}"))?;
    let (shared_session, conversation) = SharedSession::new(jsonl);

    // 4. Registry d'outils + approbateur (TUI en interactif, auto en headless).
    let headless = args.prompt.is_some();
    let (perm_tx, perm_rx) = tokio::sync::mpsc::channel(8);
    let (mode, approver): (PermissionMode, Arc<dyn agent_tools::permission::Approver>) = if headless
    {
        // -p : pas d'interlocuteur. --yes auto-accepte ; sinon refuse le sensible.
        let appr: Arc<dyn agent_tools::permission::Approver> = if args.yes {
            Arc::new(AutoApprove)
        } else {
            Arc::new(AutoDeny)
        };
        (PermissionMode::AcceptEdits, appr)
    } else {
        (PermissionMode::Default, Arc::new(TuiApprover::new(perm_tx)))
    };

    let registry = Registry::builder(&workspace)
        .mode(mode)
        .approver(approver)
        .command_hardener(harden)
        .register(Read)
        .register(Glob)
        .register(Grep)
        .register(Write)
        .register(Edit)
        .register(Bash)
        .build();
    let tool_specs = registry.tool_specs();

    // 5. Deps injectées dans la boucle.
    let deps = Deps {
        provider,
        session: shared_session,
        tokenizer: Arc::new(HeuristicCounter),
        clock: Arc::new(SystemClock),
        tools: Arc::new(registry),
    };

    // 6. Dispatch headless (-p) vs interactif.
    if let Some(prompt) = args.prompt {
        let ctx = AgentContext {
            model: args.model,
            system: Some(SYSTEM_PROMPT.to_string()),
            messages: vec![Message::user(prompt)],
            tools: tool_specs,
            config: RunConfig::default(),
        };
        let result = agent_core::run_headless(ctx, deps).await;
        print!("{}", result.text);
        if !result.text.ends_with('\n') {
            println!();
        }
        if let agent_core::HeadlessEnd::Error(e) = result.ended {
            anyhow::bail!("{e}");
        }
    } else {
        let cfg = InteractiveConfig {
            model: args.model,
            system: SYSTEM_PROMPT.to_string(),
            run_config: RunConfig::default(),
            tool_specs,
            truecolor: agent_tui::supports_truecolor(),
        };
        interactive::run(deps, conversation, perm_rx, cfg).await?;
    }
    Ok(())
}
