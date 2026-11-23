//! Daemon: um laço por par (inotify + consulta periódica de mudanças
//! remotas), socket de controle e notificações de desktop.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};
use omarchy_drive_core::config::{Config, Dirs, Settings};
use omarchy_drive_core::engine::PairEngine;
use omarchy_drive_core::state::State;
use omarchy_drive_core::status::{PairState, StatusHub};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::ipc::{Request, Response};
use crate::providers;

enum PairCmd {
    SyncNow,
    ConfirmDeletes,
}

struct PairHandle {
    tx: mpsc::Sender<PairCmd>,
    task: JoinHandle<()>,
}

struct Supervisor {
    dirs: Dirs,
    status: Arc<StatusHub>,
    cfg: Config,
    pairs: HashMap<String, PairHandle>,
}

pub fn notify_desktop(settings: &Settings, title: &str, body: &str) {
    if !settings.notifications {
        return;
    }
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "omarchy-drive", "-i", "folder-remote", title, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn looks_offline(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    ["error sending request", "dns error", "connection refused", "network is unreachable", "timed out"]
        .iter()
        .any(|k| s.contains(k))
}

impl Supervisor {
    fn new(dirs: Dirs, status: Arc<StatusHub>) -> Result<Self> {
        let cfg = Config::load(&dirs.config_file())?;
        Ok(Self { dirs, status, cfg, pairs: HashMap::new() })
    }

    fn start_all(&mut self) {
        for pair in self.cfg.pairs.clone() {
            let account = self.cfg.account(&pair.account).cloned();
            self.status.update(&pair.name, |s| {
                s.account = pair.account.clone();
                s.provider = account.as_ref().map(|a| a.provider.to_string()).unwrap_or_default();
                s.local = pair.local.display().to_string();
                s.remote = pair.remote.clone();
                s.state = if pair.paused { PairState::Paused } else { PairState::Idle };
            });
            if pair.paused {
                continue;
            }
            match self.spawn_pair(&pair) {
                Ok(h) => {
                    self.pairs.insert(pair.name.clone(), h);
                }
                Err(e) => {
                    error!(pair = %pair.name, "não foi possível iniciar: {e:#}");
                    self.status.update(&pair.name, |s| {
                        s.state = PairState::Error;
                        s.last_error = Some(format!("{e:#}"));
                    });
                }
            }
        }
    }

    fn stop_all(&mut self) {
        for (_, h) in self.pairs.drain() {
            h.task.abort();
        }
    }

    fn reload(&mut self) -> Result<()> {
        self.stop_all();
        let old: Vec<String> = self.status.snapshot().pairs.into_keys().collect();
        self.cfg = Config::load(&self.dirs.config_file())?;
        for name in old {
            if self.cfg.pair(&name).is_none() {
                self.status.remove(&name);
            }
        }
        self.start_all();
        Ok(())
    }

    fn spawn_pair(&self, pair: &omarchy_drive_core::config::SyncPair) -> Result<PairHandle> {
        let account = self
            .cfg
            .account(&pair.account)
            .with_context(|| format!("conta {} não existe", pair.account))?;
        let provider = providers::build(&self.cfg, &self.dirs, account)?;
        let state = State::open(&self.dirs.pair_db(&pair.name))?;
        let engine =
            PairEngine::new(pair.clone(), provider, state, self.cfg.settings.clone(), self.status.clone())?;
        let (tx, rx) = mpsc::channel(16);
        let settings = self.cfg.settings.clone();
        let status = self.status.clone();
        let task = tokio::spawn(pair_loop(engine, rx, settings, status));
        Ok(PairHandle { tx, task })
    }

    fn set_paused(&mut self, pair: Option<&str>, paused: bool) -> Result<String> {
        let mut n = 0;
        for p in self.cfg.pairs.iter_mut() {
            if pair.is_none_or(|x| x == p.name) {
                p.paused = paused;
                n += 1;
            }
        }
        anyhow::ensure!(n > 0, "par não encontrado");
        self.cfg.save(&self.dirs.config_file())?;
        self.reload()?;
        Ok(format!("{n} par(es) {}", if paused { "pausado(s)" } else { "retomado(s)" }))
    }

    async fn send(&self, pair: Option<&str>, cmd: impl Fn() -> PairCmd) -> Result<String> {
        let mut n = 0;
        for (name, h) in &self.pairs {
            if pair.is_none_or(|x| x == name) {
                let _ = h.tx.send(cmd()).await;
                n += 1;
            }
        }
        anyhow::ensure!(n > 0, "nenhum par ativo correspondente");
        Ok(format!("enviado para {n} par(es)"))
    }

    async fn handle(&mut self, req: Request) -> Result<String> {
        match req {
            Request::SyncNow { pair } => self.send(pair.as_deref(), || PairCmd::SyncNow).await,
            Request::ConfirmDeletes { pair } => {
                self.send(Some(&pair), || PairCmd::ConfirmDeletes).await
            }
            Request::Pause { pair } => self.set_paused(pair.as_deref(), true),
            Request::Resume { pair } => self.set_paused(pair.as_deref(), false),
            Request::ClearConflicts { pair } => {
                for name in self.status.snapshot().pairs.keys() {
                    if pair.as_deref().is_none_or(|x| x == name) {
                        self.status.update(name, |s| s.conflicts.clear());
                    }
                }
                Ok("conflitos limpos".into())
            }
            Request::Reload => self.reload().map(|_| "configuração recarregada".into()),
        }
    }
}

async fn pair_loop(
    engine: PairEngine,
    mut rx: mpsc::Receiver<PairCmd>,
    settings: Settings,
    status: Arc<StatusHub>,
) {
    let name = engine.pair.name.clone();
    // Observador do sistema de arquivos (inotify).
    let (ev_tx, mut ev_rx) = mpsc::channel::<()>(1);
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let ours = ev.paths.iter().all(|p| p.to_string_lossy().contains(".omarchy-drive"));
            if !ours {
                let _ = ev_tx.try_send(());
            }
        }
    });
    let _watcher = match watcher {
        Ok(mut w) => match w.watch(&engine.pair.local, RecursiveMode::Recursive) {
            Ok(()) => Some(w),
            Err(e) => {
                warn!(pair = %name, "inotify indisponível ({e}); só consulta periódica");
                None
            }
        },
        Err(e) => {
            warn!(pair = %name, "inotify indisponível ({e})");
            None
        }
    };

    let mut confirm = false;
    let mut backoff = Duration::ZERO;
    let mut was_error = false;
    let mut notified_hold = false;
    loop {
        let res = engine.sync_once(confirm).await;
        confirm = false;
        let mut rerun = false;
        match res {
            Ok(rep) => {
                backoff = Duration::ZERO;
                was_error = false;
                for c in &rep.conflicts {
                    notify_desktop(
                        &settings,
                        "Conflito de sincronização",
                        &format!("{c}: as duas versões foram mantidas"),
                    );
                }
                if rep.held_deletes > 0 && !notified_hold {
                    notified_hold = true;
                    notify_desktop(
                        &settings,
                        "Exclusão em massa retida",
                        &format!(
                            "{name}: {} itens seriam apagados. Confirme com `omarchy-drive confirm {name}`",
                            rep.held_deletes
                        ),
                    );
                } else if rep.held_deletes == 0 {
                    notified_hold = false;
                }
                rerun = rep.rerun;
            }
            Err(e) => {
                let offline = looks_offline(&e);
                warn!(pair = %name, "ciclo falhou: {e:#}");
                status.update(&name, |s| {
                    s.state = if offline { PairState::Offline } else { PairState::Error };
                    s.last_error = Some(format!("{e:#}"));
                    s.active.clear();
                });
                if !was_error && !offline {
                    notify_desktop(&settings, "Erro de sincronização", &format!("{name}: {e:#}"));
                }
                was_error = true;
                backoff = (backoff * 2).clamp(Duration::from_secs(5), Duration::from_secs(300));
            }
        }
        if rerun {
            continue;
        }

        let wait = if was_error { backoff } else { Duration::from_secs(settings.poll_interval_secs) };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            Some(()) = ev_rx.recv() => {
                // Espera o disco "assentar": 2,5 s sem eventos (máx. 30 s).
                let start = tokio::time::Instant::now();
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(2500)) => break,
                        Some(()) = ev_rx.recv() => {
                            if start.elapsed() > Duration::from_secs(30) { break; }
                        }
                    }
                }
            }
            cmd = rx.recv() => match cmd {
                Some(PairCmd::SyncNow) => {}
                Some(PairCmd::ConfirmDeletes) => confirm = true,
                None => return,
            },
        }
    }
}

pub async fn run(dirs: Dirs) -> Result<()> {
    std::fs::create_dir_all(&dirs.runtime)?;
    std::fs::set_permissions(&dirs.runtime, std::fs::Permissions::from_mode(0o700))?;
    let socket = dirs.socket();
    let status_file = dirs.status_file();
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).context("abrindo socket de controle")?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    let status = Arc::new(StatusHub::new(Some(status_file.clone())));
    status.flush();
    let mut sup = Supervisor::new(dirs, status)?;
    sup.start_all();
    info!(pares = sup.cfg.pairs.len(), "omarchy-drive iniciado");

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    loop {
        tokio::select! {
            conn = listener.accept() => {
                let Ok((stream, _)) = conn else { continue };
                let (r, mut w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                // Uma requisição por conexão; tratada aqui para ter &mut sup.
                let line = match tokio::time::timeout(Duration::from_secs(5), lines.next_line()).await {
                    Ok(Ok(Some(l))) => l,
                    _ => continue,
                };
                let resp = match serde_json::from_str::<Request>(&line) {
                    Ok(req) => match sup.handle(req).await {
                        Ok(m) => Response { ok: true, message: m },
                        Err(e) => Response { ok: false, message: format!("{e:#}") },
                    },
                    Err(e) => Response { ok: false, message: format!("requisição inválida: {e}") },
                };
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                let _ = w.write_all(out.as_bytes()).await;
            }
            _ = hup.recv() => {
                if let Err(e) = sup.reload() { error!("reload falhou: {e:#}"); }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
        }
    }
    info!("encerrando");
    sup.stop_all();
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&status_file);
    Ok(())
}
