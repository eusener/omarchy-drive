//! Fluxo único de autorização de contas, usado por `account connect`, `add` e
//! `reauth` (e, por eles, pelo widget da barra).
//!
//! O progresso fica em `$XDG_RUNTIME_DIR/omarchy-drive/auth.json`, que o widget
//! lê. Um `flock` em `auth.lock` garante um fluxo por vez; o lock some com o
//! processo, então um fluxo morto nunca prende os seguintes.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use omarchy_drive_core::config::{Account, Config, Dirs, ProviderKind};
use omarchy_drive_core::oauth;
use serde::{Deserialize, Serialize};

use crate::ipc::{self, Request};
use crate::providers;

/// Tempo máximo esperando o navegador devolver a autorização.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowState {
    Authorizing,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProgress {
    pub state: FlowState,
    pub provider: String,
    /// Conta envolvida (conhecida desde o início no reauth; no fim no connect).
    pub account: Option<String>,
    pub message: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

/// O que fazer com os tokens obtidos.
pub enum Target {
    /// Sem nome: reautoriza se o e-mail já existir no provedor, senão cria a conta.
    Connect,
    /// Conta nova com nome escolhido.
    Add(String),
    /// Conta existente; recusa outro e-mail sem `force`.
    Reauth { name: String, force: bool },
}

pub fn progress_file(dirs: &Dirs) -> PathBuf {
    dirs.runtime.join("auth.json")
}

fn lock_file(dirs: &Dirs) -> PathBuf {
    dirs.runtime.join("auth.lock")
}

pub fn read_progress(dirs: &Dirs) -> Option<AuthProgress> {
    serde_json::from_slice(&std::fs::read(progress_file(dirs)).ok()?).ok()
}

fn write_progress(dirs: &Dirs, p: &AuthProgress) {
    let path = progress_file(dirs);
    let tmp = path.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec_pretty(p)
        && std::fs::write(&tmp, b).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Lock exclusivo, sem esperar. Mantido enquanto o `File` viver.
fn try_lock(path: &Path) -> Result<Option<std::fs::File>> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    // SAFETY: descritor válido enquanto `f` existir.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok(if r == 0 { Some(f) } else { None })
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: sinal 0 só verifica a existência do processo.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Executa o fluxo inteiro e devolve a mensagem final de sucesso.
pub async fn run(kind: ProviderKind, target: Target) -> Result<String> {
    let dirs = Dirs::get()?;
    std::fs::create_dir_all(&dirs.runtime)?;
    let Some(_lock) = try_lock(&lock_file(&dirs))? else {
        bail!("já existe uma autorização em andamento (cancele com `omarchy-drive account cancel`)");
    };

    let mut progress = AuthProgress {
        state: FlowState::Authorizing,
        provider: kind.to_string(),
        account: match &target {
            Target::Connect => None,
            Target::Add(n) | Target::Reauth { name: n, .. } => Some(n.clone()),
        },
        message: "Aguardando o navegador…".into(),
        pid: std::process::id(),
        started_at: Utc::now(),
    };
    write_progress(&dirs, &progress);

    let res = authorize_and_store(&dirs, kind, target, &mut progress).await;
    match &res {
        Ok(msg) => {
            progress.state = FlowState::Done;
            progress.message = msg.clone();
        }
        Err(e) => {
            progress.state = FlowState::Failed;
            progress.message = format!("{e:#}");
        }
    }
    write_progress(&dirs, &progress);
    res
}

async fn authorize_and_store(
    dirs: &Dirs,
    kind: ProviderKind,
    target: Target,
    progress: &mut AuthProgress,
) -> Result<String> {
    let mut cfg = Config::load(&dirs.config_file())?;
    // Validações que não dependem do login vêm antes de abrir o navegador.
    // Reauth mantém o client que autorizou a conta; conta nova usa o
    // configurado em `[oauth]` ou, sem ele, o padrão embutido.
    let client = match &target {
        Target::Add(name) if cfg.account(name).is_some() => bail!("já existe uma conta chamada {name}"),
        Target::Reauth { name, .. } => {
            let a = cfg.account(name).with_context(|| format!("conta {name} não encontrada"))?;
            anyhow::ensure!(a.provider == kind, "a conta {name} é do provedor {}", a.provider);
            providers::client_for_account(&cfg, a)?
        }
        _ => providers::client_for_new(&cfg, kind)?,
    };
    let ep = providers::endpoints(kind, &client);

    let tokens = match tokio::time::timeout(AUTH_TIMEOUT, oauth::authorize_interactive(&ep)).await {
        Ok(r) => r?,
        Err(_) => bail!("tempo esgotado esperando a autorização no navegador"),
    };

    // Os tokens ficam num arquivo provisório até sabermos de qual conta são.
    std::fs::create_dir_all(dirs.state.join("tokens"))?;
    let staged = dirs.state.join("tokens").join(format!(".pendente-{}.json", std::process::id()));
    oauth::save_tokens(&staged, &tokens)?;
    let result = finish(dirs, &mut cfg, kind, target, &client, &staged, progress).await;
    let _ = std::fs::remove_file(&staged);
    result
}

async fn finish(
    dirs: &Dirs,
    cfg: &mut Config,
    kind: ProviderKind,
    target: Target,
    client: &providers::Client,
    staged: &Path,
    progress: &mut AuthProgress,
) -> Result<String> {
    let email = providers::email_from_tokens(kind, providers::endpoints(kind, client), staged.to_path_buf())
        .await
        .context("não foi possível confirmar a conta autorizada")?;

    let (name, reauth) = match target {
        Target::Connect => match cfg.account_by_email(kind, &email) {
            Some(a) => (a.name.clone(), true),
            None => (cfg.account_name_for(&email), false),
        },
        Target::Add(name) => (name, false),
        Target::Reauth { name, force } => {
            let label = cfg.account(&name).map(|a| a.label.clone()).unwrap_or_default();
            if !label.is_empty() && !email.eq_ignore_ascii_case(&label) && !force {
                bail!(
                    "você entrou como {email}, mas a conta {name} é {label}. Nada foi alterado \
                     (use --force se a troca for intencional)"
                );
            }
            (name, true)
        }
    };
    progress.account = Some(name.clone());

    std::fs::rename(staged, dirs.token_file(&name))?;
    // Connect que virou reauth troca o client da conta pelo usado agora.
    match cfg.accounts.iter_mut().find(|a| a.name == name) {
        Some(a) => {
            a.label = email.clone();
            a.client_id = Some(client.id.clone());
        }
        None => cfg.accounts.push(Account {
            name: name.clone(),
            provider: kind,
            label: email.clone(),
            client_id: Some(client.id.clone()),
        }),
    }
    cfg.save(&dirs.config_file())?;
    if dirs.socket().exists() {
        let _ = ipc::call(&dirs.socket(), &Request::Reload).await;
    }
    Ok(if reauth {
        format!("Conta {name} ({kind}) reautorizada: {email}")
    } else {
        format!("Conta {name} ({kind}) conectada: {email}")
    })
}

/// Cancela o fluxo em andamento (se houver). Devolve `false` se não havia nenhum.
pub fn cancel() -> Result<bool> {
    let dirs = Dirs::get()?;
    let Some(mut p) = read_progress(&dirs) else { return Ok(false) };
    if p.state != FlowState::Authorizing || !pid_alive(p.pid) {
        return Ok(false);
    }
    p.state = FlowState::Cancelled;
    p.message = "Autorização cancelada".into();
    write_progress(&dirs, &p);
    // SAFETY: SIGTERM para o processo registrado no arquivo de progresso.
    unsafe {
        libc::kill(p.pid as i32, libc::SIGTERM);
    }
    Ok(true)
}
