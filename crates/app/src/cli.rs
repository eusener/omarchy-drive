use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use omarchy_drive_core::config::{Config, Dirs, ProviderKind, SyncPair, expand_tilde};
use omarchy_drive_core::engine::PairEngine;
use omarchy_drive_core::state::State;
use omarchy_drive_core::status::{PairState, StatusDoc, StatusHub};

use crate::ipc::{self, Request};
use crate::auth_flow::{self, Target};
use crate::{daemon, providers};

#[derive(Parser)]
#[command(name = "omarchy-drive", version, about = "Sincronização bidirecional de Google Drive e OneDrive para o Omarchy")]
pub struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

impl Cli {
    pub fn is_daemon(&self) -> bool {
        matches!(self.cmd, Cmd::Daemon)
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Configura as credenciais OAuth do app (uma vez por provedor).
    #[command(subcommand)]
    Setup(SetupCmd),
    /// Gerencia contas (Google/Microsoft).
    #[command(subcommand)]
    Account(AccountCmd),
    /// Gerencia pares de sincronização (pasta local ↔ pasta remota).
    #[command(subcommand)]
    Pair(PairCmd),
    /// Mostra o status (lido do daemon).
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Sincroniza agora. Sem daemon rodando, executa um ciclo neste processo.
    Sync {
        pair: Option<String>,
        /// Libera exclusões em massa retidas pela proteção.
        #[arg(long)]
        confirm_deletes: bool,
    },
    /// Libera uma exclusão em massa retida.
    Confirm { pair: String },
    /// Pausa um par (ou todos). Persiste entre reinícios.
    Pause { pair: Option<String> },
    /// Retoma um par (ou todos).
    Resume { pair: Option<String> },
    /// Lista os conflitos registrados (`--clear` limpa a lista).
    Conflicts {
        #[arg(long)]
        clear: bool,
    },
    /// Abre a pasta local do par no gerenciador de arquivos.
    Open { pair: Option<String> },
    /// Roda o daemon em primeiro plano (use o serviço systemd).
    Daemon,
}

#[derive(Subcommand)]
enum SetupCmd {
    /// Client OAuth do tipo "Desktop app" criado no Google Cloud Console.
    Google {
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        client_secret: String,
    },
    /// App registrado no Microsoft Entra (public client, redirect http://localhost).
    Microsoft {
        #[arg(long)]
        client_id: String,
    },
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Conecta uma conta pelo navegador; o nome vem do e-mail. Se a conta já
    /// existir, só renova o login.
    Connect {
        #[arg(long, value_enum)]
        provider: ProviderArg,
    },
    /// Cancela uma autorização em andamento.
    Cancel,
    /// Autoriza uma conta pelo navegador, com nome escolhido.
    Add {
        name: String,
        #[arg(long, value_enum)]
        provider: ProviderArg,
    },
    /// Autoriza de novo uma conta existente (token vencido ou revogado), sem mexer nos pares.
    Reauth {
        name: String,
        /// Aceita uma conta do provedor diferente da autorizada antes.
        #[arg(long)]
        force: bool,
    },
    List,
    /// Remove a conta e seus tokens. Sem `--with-pairs`, recusa se a conta
    /// ainda tiver pares; com a flag, remove os pares junto (as pastas e os
    /// arquivos locais e remotos são mantidos).
    Remove {
        name: String,
        #[arg(long)]
        with_pairs: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ProviderArg {
    Gdrive,
    Onedrive,
}

impl From<ProviderArg> for ProviderKind {
    fn from(p: ProviderArg) -> Self {
        match p {
            ProviderArg::Gdrive => ProviderKind::Gdrive,
            ProviderArg::Onedrive => ProviderKind::Onedrive,
        }
    }
}

#[derive(Subcommand)]
enum PairCmd {
    Add {
        name: String,
        #[arg(long)]
        account: String,
        /// Pasta local (ex.: ~/GoogleDrive).
        #[arg(long)]
        local: PathBuf,
        /// Pasta no drive ("" = raiz).
        #[arg(long, default_value = "")]
        remote: String,
        /// Sync seletivo: só estes subcaminhos (repetível).
        #[arg(long)]
        include: Vec<String>,
        /// Padrões glob extras a ignorar (repetível).
        #[arg(long)]
        ignore: Vec<String>,
        /// Só baixa; nunca altera o remoto.
        #[arg(long)]
        download_only: bool,
    },
    List,
    /// Remove o par (os arquivos locais e remotos são mantidos).
    Remove { name: String },
}

fn load() -> Result<(Dirs, Config)> {
    let dirs = Dirs::get()?;
    let cfg = Config::load(&dirs.config_file())?;
    Ok((dirs, cfg))
}

/// Apaga o estado SQLite de um par removido.
fn remove_pair_db(dirs: &Dirs, pair: &str) {
    let db = dirs.pair_db(pair);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
}

/// Avisa o daemon (se estiver rodando) que a configuração mudou.
async fn notify_reload(dirs: &Dirs) {
    if dirs.socket().exists() {
        let _ = ipc::call(&dirs.socket(), &Request::Reload).await;
    }
}

async fn daemon_call(dirs: &Dirs, req: Request) -> Result<()> {
    let r = ipc::call(&dirs.socket(), &req).await?;
    if r.ok {
        println!("{}", r.message);
        Ok(())
    } else {
        bail!(r.message)
    }
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Setup(s) => {
            let (dirs, mut cfg) = load()?;
            match s {
                SetupCmd::Google { client_id, client_secret } => {
                    cfg.oauth.google_client_id = Some(client_id);
                    cfg.oauth.google_client_secret = Some(client_secret);
                }
                SetupCmd::Microsoft { client_id } => cfg.oauth.microsoft_client_id = Some(client_id),
            }
            cfg.save(&dirs.config_file())?;
            println!("Credenciais salvas em {}", dirs.config_file().display());
        }

        Cmd::Account(AccountCmd::Connect { provider }) => {
            println!("{}", auth_flow::run(provider.into(), Target::Connect).await?);
        }
        Cmd::Account(AccountCmd::Cancel) => {
            if auth_flow::cancel()? {
                println!("Autorização cancelada.");
            } else {
                println!("Nenhuma autorização em andamento.");
            }
        }
        Cmd::Account(AccountCmd::Add { name, provider }) => {
            println!("{}", auth_flow::run(provider.into(), Target::Add(name)).await?);
        }
        Cmd::Account(AccountCmd::Reauth { name, force }) => {
            let (_, cfg) = load()?;
            let kind = cfg.account(&name).with_context(|| format!("conta {name} não encontrada"))?.provider;
            println!("{}", auth_flow::run(kind, Target::Reauth { name, force }).await?);
        }
        Cmd::Account(AccountCmd::List) => {
            let (_, cfg) = load()?;
            if cfg.accounts.is_empty() {
                println!("Nenhuma conta. Use `omarchy-drive account connect --provider gdrive|onedrive` ou o painel da barra.");
            }
            for a in &cfg.accounts {
                println!("{:<16} {:<9} {}", a.name, a.provider.to_string(), a.label);
            }
        }
        Cmd::Account(AccountCmd::Remove { name, with_pairs }) => {
            let (dirs, mut cfg) = load()?;
            anyhow::ensure!(cfg.account(&name).is_some(), "conta {name} não encontrada");
            let pairs: Vec<String> =
                cfg.pairs.iter().filter(|p| p.account == name).map(|p| p.name.clone()).collect();
            if !pairs.is_empty() && !with_pairs {
                bail!(
                    "a conta ainda é usada por: {}. Remova os pares antes ou use --with-pairs",
                    pairs.join(", ")
                );
            }
            cfg.pairs.retain(|p| p.account != name);
            cfg.accounts.retain(|a| a.name != name);
            cfg.save(&dirs.config_file())?;
            for p in &pairs {
                remove_pair_db(&dirs, p);
            }
            let _ = std::fs::remove_file(dirs.token_file(&name));
            notify_reload(&dirs).await;
            if pairs.is_empty() {
                println!("Conta {name} removida.");
            } else {
                println!("Conta {name} removida com os pares {} (arquivos mantidos nos dois lados).", pairs.join(", "));
            }
        }

        Cmd::Pair(PairCmd::Add { name, account, local, remote, include, ignore, download_only }) => {
            let (dirs, mut cfg) = load()?;
            if cfg.pair(&name).is_some() {
                bail!("já existe um par chamado {name}");
            }
            let acc = cfg.account(&account).with_context(|| format!("conta {account} não existe"))?.clone();
            let local = std::path::absolute(expand_tilde(&local))?;
            if let Some(other) = cfg.pairs.iter().find(|p| {
                p.local.starts_with(&local) || local.starts_with(&p.local)
            }) {
                bail!("a pasta se sobrepõe ao par {} ({})", other.name, other.local.display());
            }
            std::fs::create_dir_all(&local)?;
            let remote = remote.trim_matches('/').to_string();
            // Valida (e cria, se preciso) a pasta remota agora, para errar cedo.
            let provider = providers::build(&cfg, &dirs, &acc)?;
            let root = provider.resolve_folder(&remote).await?;
            println!("Pasta remota \"{}\" ok (id {}).", if remote.is_empty() { "/" } else { &remote }, root.id);
            cfg.pairs.push(SyncPair {
                name: name.clone(),
                account,
                local: local.clone(),
                remote,
                include,
                ignore,
                download_only,
                paused: false,
            });
            cfg.save(&dirs.config_file())?;
            println!("Par {name} criado: {}", local.display());
            notify_reload(&dirs).await;
        }
        Cmd::Pair(PairCmd::List) => {
            let (_, cfg) = load()?;
            if cfg.pairs.is_empty() {
                println!("Nenhum par. Use `omarchy-drive pair add`.");
            }
            for p in &cfg.pairs {
                let flags = [
                    p.paused.then_some("pausado"),
                    p.download_only.then_some("só download"),
                    (!p.include.is_empty()).then_some("seletivo"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
                println!(
                    "{:<14} {:<12} {} ↔ /{} {}",
                    p.name,
                    p.account,
                    p.local.display(),
                    p.remote,
                    if flags.is_empty() { String::new() } else { format!("[{flags}]") }
                );
            }
        }
        Cmd::Pair(PairCmd::Remove { name }) => {
            let (dirs, mut cfg) = load()?;
            let before = cfg.pairs.len();
            cfg.pairs.retain(|p| p.name != name);
            if cfg.pairs.len() == before {
                bail!("par {name} não encontrado");
            }
            cfg.save(&dirs.config_file())?;
            remove_pair_db(&dirs, &name);
            println!("Par {name} removido (arquivos mantidos nos dois lados).");
            notify_reload(&dirs).await;
        }

        Cmd::Status { json } => {
            let dirs = Dirs::get()?;
            let doc: Option<StatusDoc> = std::fs::read(dirs.status_file())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            match (json, doc) {
                (true, Some(d)) => println!("{}", serde_json::to_string_pretty(&d)?),
                (true, None) => println!("{{\"running\": false}}"),
                (false, None) => println!("Daemon parado. Inicie com `systemctl --user enable --now omarchy-drive`."),
                (false, Some(d)) => print_status(&d),
            }
        }

        Cmd::Sync { pair, confirm_deletes } => {
            let (dirs, cfg) = load()?;
            if dirs.socket().exists() && ipc::call(&dirs.socket(), &Request::Reload).await.is_ok() {
                if confirm_deletes {
                    let p = pair.context("informe o par para confirmar exclusões")?;
                    return daemon_call(&dirs, Request::ConfirmDeletes { pair: p }).await;
                }
                return daemon_call(&dirs, Request::SyncNow { pair }).await;
            }
            // Sem daemon: um ciclo aqui mesmo.
            let status = Arc::new(StatusHub::new(None));
            let mut ran = 0;
            for p in cfg.pairs.iter().filter(|p| pair.as_deref().is_none_or(|x| x == p.name)) {
                let acc = cfg.account(&p.account).with_context(|| format!("conta {} não existe", p.account))?;
                let provider = providers::build(&cfg, &dirs, acc)?;
                let engine = PairEngine::new(
                    p.clone(),
                    provider,
                    State::open(&dirs.pair_db(&p.name))?,
                    cfg.settings.clone(),
                    status.clone(),
                )?;
                loop {
                    let r = engine.sync_once(confirm_deletes).await?;
                    println!(
                        "{}: {} ações, {} falhas{}",
                        p.name,
                        r.executed,
                        r.failed,
                        if r.held_deletes > 0 {
                            format!(", {} exclusões retidas (use --confirm-deletes)", r.held_deletes)
                        } else {
                            String::new()
                        }
                    );
                    if !r.rerun {
                        break;
                    }
                }
                ran += 1;
            }
            if ran == 0 {
                bail!("nenhum par encontrado");
            }
        }
        Cmd::Confirm { pair } => {
            let dirs = Dirs::get()?;
            daemon_call(&dirs, Request::ConfirmDeletes { pair }).await?;
        }
        Cmd::Pause { pair } => daemon_call(&Dirs::get()?, Request::Pause { pair }).await?,
        Cmd::Resume { pair } => daemon_call(&Dirs::get()?, Request::Resume { pair }).await?,
        Cmd::Conflicts { clear } => {
            let dirs = Dirs::get()?;
            if clear {
                return daemon_call(&dirs, Request::ClearConflicts { pair: None }).await;
            }
            let doc: StatusDoc = serde_json::from_slice(
                &std::fs::read(dirs.status_file()).context("daemon parado")?,
            )?;
            let mut any = false;
            for (name, p) in &doc.pairs {
                for c in &p.conflicts {
                    any = true;
                    println!("{name}: {}/{c}", p.local);
                }
            }
            if !any {
                println!("Nenhum conflito.");
            }
        }
        Cmd::Open { pair } => {
            let (_, cfg) = load()?;
            let p = match pair {
                Some(n) => cfg.pair(&n).with_context(|| format!("par {n} não encontrado"))?,
                None => cfg.pairs.first().context("nenhum par configurado")?,
            };
            std::process::Command::new("xdg-open").arg(&p.local).spawn()?;
        }
        Cmd::Daemon => daemon::run(Dirs::get()?).await?,
    }
    Ok(())
}

fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

fn print_status(d: &StatusDoc) {
    if d.pairs.is_empty() {
        println!("Daemon rodando, nenhum par configurado.");
    }
    for (name, p) in &d.pairs {
        let state = match p.state {
            PairState::Idle => "em dia",
            PairState::Syncing => "sincronizando",
            PairState::Paused => "pausado",
            PairState::Offline => "offline",
            PairState::Error => "erro",
            PairState::NeedsConfirm => "aguardando confirmação",
            PairState::AuthExpired => "login expirado",
        };
        println!("● {name} [{}] — {state}", p.provider);
        println!("  {} ↔ /{}", p.local, p.remote);
        if let Some(t) = p.last_sync {
            println!("  última sincronização: {}", t.with_timezone(&chrono::Local).format("%d/%m %H:%M:%S"));
        }
        if p.queued > 0 {
            println!("  na fila: {}  transferindo: {}", p.queued, p.active.join(", "));
        }
        if let (Some(u), Some(t)) = (p.quota_used, p.quota_total) {
            println!("  armazenamento: {} de {}", human(u), human(t));
        }
        if p.pending_deletes > 0 {
            println!("  ⚠ {} exclusões retidas: `omarchy-drive confirm {name}`", p.pending_deletes);
        }
        if !p.conflicts.is_empty() {
            println!("  ⚠ {} conflito(s): `omarchy-drive conflicts`", p.conflicts.len());
        }
        if let Some(e) = &p.last_error {
            println!("  último erro: {e}");
        }
        for w in &p.warnings {
            println!("  aviso: {w}");
        }
    }
}
