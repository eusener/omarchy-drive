//! Motor de sincronização de um par: atualiza a árvore remota, varre o disco,
//! planeja e executa as ações com verificações de segurança.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::config::{Settings, SyncPair};
use crate::hash::hash_file;
use crate::ignore::Ignore;
use crate::model::*;
use crate::planner::{self, Action, Plan, Snapshot};
use crate::provider::Provider;
use crate::scan;
use crate::state::State;
use crate::status::{PairState, StatusHub};
use crate::tree::RemoteTree;

const KV_ROOT: &str = "root_id";
const KV_CURSOR: &str = "cursor";
const KV_ROOT_PATH: &str = "root_path";

/// O que reler do disco num ciclo.
#[derive(Debug, Clone)]
pub enum ScanScope {
    /// Pasta inteira.
    Full,
    /// Só estes caminhos (relativos, com subárvores), sobre o último estado
    /// conhecido. Vazio = não lê o disco (ciclo disparado só pelo remoto).
    Paths(HashSet<RelPath>),
}

/// Reconciliação completa periódica, mesmo sem eventos.
pub const FULL_SCAN_EVERY: Duration = Duration::from_secs(6 * 3600);

/// Último estado local conhecido, base da varredura incremental.
#[derive(Default)]
struct LocalCache {
    entries: Option<HashMap<RelPath, LocalEntry>>,
    last_full: Option<std::time::Instant>,
    /// Caminhos que o próprio motor tocou ou não conseguiu ler: entram na
    /// próxima varredura parcial.
    dirty: HashSet<RelPath>,
    force_full: bool,
}

#[derive(Debug, Default)]
pub struct CycleReport {
    pub executed: usize,
    pub failed: usize,
    pub held_deletes: usize,
    pub conflicts: Vec<RelPath>,
    /// Algo mudou que pede um novo ciclo imediato (ex.: cópia de conflito
    /// criada que ainda precisa subir).
    pub rerun: bool,
}

pub struct PairEngine {
    pub pair: SyncPair,
    provider: Arc<dyn Provider>,
    state: Mutex<State>,
    ignore: Ignore,
    settings: Settings,
    status: Arc<StatusHub>,
    trash_dir: Option<PathBuf>,
    last_quota: Mutex<Option<std::time::Instant>>,
    cache: Mutex<LocalCache>,
}

/// Cota muda devagar: consulta no máximo a cada 10 minutos.
const QUOTA_EVERY: Duration = Duration::from_secs(600);

/// Contexto mutável de um ciclo, compartilhado entre transferências paralelas.
struct Cycle {
    root_id: String,
    base: Mutex<HashMap<RelPath, BaseEntry>>,
    local: HashMap<RelPath, LocalEntry>,
    remote: HashMap<RelPath, RemoteEntry>,
}

/// Sufixo único para arquivos temporários: downloads paralelos na mesma pasta
/// não podem compartilhar o mesmo temporário.
fn tmp_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}-{}", now_ms(), std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.mtime() * 1_000_000_000 + md.mtime_nsec()
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "linux".into())
}

/// `relatorio.pdf` → `relatorio (conflito host 2026-11-23 10-15-02).pdf`
pub fn conflict_name(name: &str) -> String {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
    let tag = format!("conflito {} {stamp}", hostname());
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({tag}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({tag})"),
    }
}

impl PairEngine {
    pub fn new(
        pair: SyncPair,
        provider: Arc<dyn Provider>,
        state: State,
        settings: Settings,
        status: Arc<StatusHub>,
    ) -> Result<Self> {
        let ignore = Ignore::new(&pair.ignore, &pair.include)?;
        let trash_dir = crate::trash::default_trash_dir().ok();
        Ok(Self {
            pair,
            provider,
            state: Mutex::new(state),
            ignore,
            settings,
            status,
            trash_dir,
            last_quota: Mutex::new(None),
            cache: Mutex::new(LocalCache::default()),
        })
    }

    /// Troca a lixeira local (usado nos testes).
    pub fn with_trash_dir(mut self, dir: PathBuf) -> Self {
        self.trash_dir = Some(dir);
        self
    }

    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    fn st<T>(&self, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        f(&mut self.state.lock().unwrap())
    }

    fn abs(&self, rel: &str) -> PathBuf {
        if rel.is_empty() { self.pair.local.clone() } else { self.pair.local.join(rel) }
    }

    /// Caminhos que as ações vão mexer na pasta local entram na próxima
    /// varredura parcial (o inotify também os vê, mas não dependemos só dele).
    /// Ações só no remoto ou na base não mudam o disco. Um conflito cria uma
    /// cópia com nome novo: a próxima varredura é completa.
    fn mark_touched(&self, actions: &[Action]) {
        let mut c = self.cache.lock().unwrap();
        for a in actions {
            match a {
                Action::MoveLocal { from, to } => {
                    c.dirty.insert(from.clone());
                    c.dirty.insert(to.clone());
                }
                Action::Conflict { path } => {
                    c.dirty.insert(path.clone());
                    c.force_full = true;
                }
                Action::Download { path } | Action::CreateLocalDir { path } | Action::DeleteLocal { path } => {
                    c.dirty.insert(path.clone());
                }
                Action::Upload { .. }
                | Action::MoveRemote { .. }
                | Action::CreateRemoteDir { .. }
                | Action::DeleteRemote { .. }
                | Action::RecordBase { .. }
                | Action::ForgetBase { .. } => {}
            }
        }
    }

    fn set_status(&self, f: impl FnOnce(&mut crate::status::PairStatus)) {
        self.status.update(&self.pair.name, f);
    }

    // ------------------------------------------------------------------
    // Árvore remota
    // ------------------------------------------------------------------

    async fn root_id(&self) -> Result<String> {
        let saved = self.st(|s| Ok((s.get(KV_ROOT)?, s.get(KV_ROOT_PATH)?)))?;
        if let (Some(id), Some(path)) = saved
            && path == self.pair.remote
        {
            return Ok(id);
        }
        let root = self.provider.resolve_folder(&self.pair.remote).await?;
        // Raiz mudou de lugar na config: recomeça do zero (a base antiga não
        // vale mais).
        self.st(|s| {
            s.set(KV_ROOT, &root.id)?;
            s.set(KV_ROOT_PATH, &self.pair.remote)?;
            s.del(KV_CURSOR)
        })?;
        Ok(root.id)
    }

    async fn full_listing(&self, root_id: &str) -> Result<()> {
        info!(pair = %self.pair.name, "listagem remota completa");
        let cursor = self.provider.start_cursor(root_id).await?;
        let items = self.provider.list_tree(root_id).await?;
        self.st(|s| {
            s.replace_remote(&items)?;
            s.set(KV_CURSOR, &cursor)
        })
    }

    async fn refresh_remote(&self, root_id: &str) -> Result<RemoteTree> {
        let cursor = self.st(|s| s.get(KV_CURSOR))?;
        match cursor {
            None => self.full_listing(root_id).await?,
            Some(c) => {
                let batch = self.provider.changes(root_id, &c).await?;
                if batch.reset {
                    self.full_listing(root_id).await?;
                } else {
                    let before = RemoteTree::new(root_id.to_string(), self.st(|s| s.load_remote())?);
                    self.st(|s| {
                        s.remove_remote(&batch.removed)?;
                        s.upsert_remote(&batch.upserts)
                    })?;
                    // Pastas que entraram na raiz vindas de fora (movidas para
                    // dentro) chegam sem os filhos: lista a subárvore delas.
                    let after = RemoteTree::new(root_id.to_string(), self.st(|s| s.load_remote())?);
                    for it in batch.upserts.iter().filter(|i| i.is_dir) {
                        if before.path_of(&it.id).is_none() && after.path_of(&it.id).is_some() {
                            let sub = self.provider.list_tree(&it.id).await?;
                            self.st(|s| s.upsert_remote(&sub))?;
                        }
                    }
                    self.st(|s| s.set(KV_CURSOR, &batch.cursor))?;
                }
            }
        }
        Ok(RemoteTree::new(root_id.to_string(), self.st(|s| s.load_remote())?))
    }

    // ------------------------------------------------------------------
    // Ciclo
    // ------------------------------------------------------------------

    /// Executa um ciclo completo. `confirm_deletes` libera exclusões em massa
    /// retidas pela proteção.
    /// Um ciclo completo. Login recusado pelo provedor deixa o par em
    /// `AuthExpired` (o erro continua sendo devolvido ao chamador).
    pub async fn sync_once(&self, confirm_deletes: bool) -> Result<CycleReport> {
        self.sync_scoped(confirm_deletes, ScanScope::Full).await
    }

    /// Ciclo com varredura limitada a `scope` (o motor ainda decide fazer a
    /// completa quando não há estado anterior ou ela está vencida).
    pub async fn sync_scoped(&self, confirm_deletes: bool, scope: ScanScope) -> Result<CycleReport> {
        // Registrado antes de qualquer passo que possa falhar (rede, por
        // exemplo): um ciclo que falha não perde os caminhos pendentes.
        {
            let mut c = self.cache.lock().unwrap();
            match scope {
                ScanScope::Full => c.force_full = true,
                ScanScope::Paths(p) => c.dirty.extend(p),
            }
        }
        let res = self.sync_inner(confirm_deletes).await;
        if let Err(e) = &res
            && crate::oauth::is_auth_expired(e)
        {
            let msg = format!("{e:#}");
            self.set_status(|s| {
                s.state = PairState::AuthExpired;
                s.last_error = Some(msg);
                s.active.clear();
                s.queued = 0;
            });
        }
        res
    }

    async fn sync_inner(&self, confirm_deletes: bool) -> Result<CycleReport> {
        let local_root = &self.pair.local;
        // Pasta local ausente (disco não montado, pasta apagada) nunca pode
        // virar "apagar tudo no remoto".
        if !local_root.is_dir() {
            bail!("pasta local {} não existe ou não está acessível", local_root.display());
        }

        self.set_status(|s| {
            s.state = PairState::Syncing;
            s.last_error = None;
        });

        let root_id = self.root_id().await?;
        let tree = self.refresh_remote(&root_id).await?;
        let (mut remote, dups) = tree.entries(&self.ignore);

        let mut base = self.st(|s| s.load_base())?;
        let ignore = self.ignore.clone();
        let base_for_scan = base.clone();
        let kind = self.provider.hash_kind();
        let root = local_root.clone();
        let (prev, dirty) = {
            let mut c = self.cache.lock().unwrap();
            let due = c.last_full.is_none_or(|t| t.elapsed() >= FULL_SCAN_EVERY);
            let full = self.pair.full_scan_only || c.force_full || due;
            c.force_full = false;
            let dirty = std::mem::take(&mut c.dirty);
            (if full { None } else { c.entries.clone() }, dirty)
        };
        let full = prev.is_none();
        if full {
            info!(pair = %self.pair.name, scope = "full", "varredura local");
        } else if !dirty.is_empty() {
            info!(pair = %self.pair.name, scope = "paths", n = dirty.len(), "varredura local");
        }
        let dirty_for_scan = dirty.clone();
        let scanned = tokio::task::spawn_blocking(move || match prev {
            None => scan::scan(&root, &ignore, &base_for_scan, kind),
            Some(prev) if dirty_for_scan.is_empty() => scan::ScanResult { entries: prev, ..Default::default() },
            Some(prev) => scan::scan_paths(&root, &ignore, &base_for_scan, kind, &prev, &dirty_for_scan),
        })
        .await?;
        if scanned.skipped.iter().any(|p| p.is_empty()) {
            // Mantém os caminhos pendentes para a próxima tentativa.
            let mut c = self.cache.lock().unwrap();
            c.dirty.extend(dirty);
            c.force_full |= full;
            drop(c);
            bail!("não foi possível ler a pasta local {}", local_root.display());
        }
        {
            let mut c = self.cache.lock().unwrap();
            c.entries = Some(scanned.entries.clone());
            if full {
                c.last_full = Some(std::time::Instant::now());
            }
            c.dirty.extend(scanned.skipped.iter().cloned());
        }
        for e in &scanned.errors {
            warn!(pair = %self.pair.name, "{e}");
        }
        let mut local = scanned.entries;

        // Exclui do ciclo tudo cujo estado local é desconhecido agora.
        if !scanned.skipped.is_empty() {
            let skip = |p: &str| {
                scanned.skipped.iter().any(|s| p == s || is_under(p, s))
            };
            base.retain(|p, _| !skip(p));
            local.retain(|p, _| !skip(p));
            remote.retain(|p, _| !skip(p));
        }

        let snap = Snapshot { base: base.clone(), local, remote };
        let mut plan = planner::plan(&snap);
        let plan_warnings = std::mem::take(&mut plan.warnings);
        self.mark_touched(&plan.actions);
        if self.pair.download_only {
            restrict_download_only(&mut plan);
        }

        let mut report = CycleReport::default();
        let deletes = plan.local_deletes() + plan.remote_deletes();
        if deletes > self.settings.max_deletes_without_confirm && !confirm_deletes {
            warn!(pair = %self.pair.name, deletes, "exclusão em massa retida aguardando confirmação");
            plan.actions
                .retain(|a| !matches!(a, Action::DeleteLocal { .. } | Action::DeleteRemote { .. }));
            report.held_deletes = deletes;
        }

        let local = std::mem::take(&mut plan.local);
        let remote = std::mem::take(&mut plan.remote);
        let cycle = Arc::new(Cycle { root_id, base: Mutex::new(base), local, remote });

        let total = plan.actions.len();
        self.set_status(|s| s.queued = total);
        if total > 0 {
            info!(pair = %self.pair.name, actions = total, "executando plano");
        }

        // Fases 0–1 (moves e pastas) em sequência; fase 2 (transferências) em
        // paralelo; fases 3–4 (exclusões) em sequência.
        let mut transfers = Vec::new();
        let mut tail = Vec::new();
        for a in plan.actions {
            match a {
                Action::MoveLocal { .. }
                | Action::MoveRemote { .. }
                | Action::CreateLocalDir { .. }
                | Action::CreateRemoteDir { .. } => self.run(&cycle, a, &mut report).await,
                Action::Download { .. }
                | Action::Upload { .. }
                | Action::Conflict { .. }
                | Action::RecordBase { .. } => transfers.push(a),
                _ => tail.push(a),
            }
        }

        let par = self.settings.parallel_transfers.max(1);
        let results: Vec<(Action, Result<()>)> = futures::stream::iter(transfers)
            .map(|a| {
                let cycle = cycle.clone();
                async move {
                    let r = self.exec(&cycle, &a).await;
                    (a, r)
                }
            })
            .buffer_unordered(par)
            .collect()
            .await;
        for (a, r) in results {
            self.account(&a, r, &mut report);
        }
        for a in tail {
            self.run(&cycle, a, &mut report).await;
        }

        let due = self.last_quota.lock().unwrap().is_none_or(|t| t.elapsed() >= QUOTA_EVERY);
        let quota = if due { self.provider.quota().await.ok() } else { None };
        if quota.is_some() {
            *self.last_quota.lock().unwrap() = Some(std::time::Instant::now());
        }
        let held = report.held_deletes;
        let conflicts = report.conflicts.clone();
        self.set_status(|s| {
            s.state = if held > 0 { PairState::NeedsConfirm } else { PairState::Idle };
            s.pending_deletes = held;
            s.queued = 0;
            s.active.clear();
            s.last_sync = Some(chrono::Utc::now());
            s.warnings = dups.iter().map(|d| format!("nomes duplicados no remoto: {d}")).collect();
            s.warnings.extend(plan_warnings.iter().cloned());
            for c in conflicts {
                if !s.conflicts.contains(&c) {
                    s.conflicts.push(c);
                }
            }
            if let Some(q) = quota {
                s.quota_used = Some(q.used);
                s.quota_total = q.total;
            }
        });
        Ok(report)
    }

    async fn run(&self, cycle: &Arc<Cycle>, a: Action, report: &mut CycleReport) {
        let r = self.exec(cycle, &a).await;
        self.account(&a, r, report);
    }

    fn account(&self, a: &Action, r: Result<()>, report: &mut CycleReport) {
        match r {
            Ok(()) => {
                report.executed += 1;
                match a {
                    Action::Conflict { path } => {
                        report.conflicts.push(path.clone());
                        report.rerun = true;
                    }
                    Action::Upload { .. } => self.set_status(|s| s.uploaded += 1),
                    Action::Download { .. } => self.set_status(|s| s.downloaded += 1),
                    _ => {}
                }
            }
            Err(e) => {
                report.failed += 1;
                let msg = format!("{}: {e:#}", a.path());
                warn!(pair = %self.pair.name, "{msg}");
                self.set_status(|s| s.last_error = Some(msg));
            }
        }
        self.set_status(|s| s.queued = s.queued.saturating_sub(1));
    }

    // ------------------------------------------------------------------
    // Execução de ações
    // ------------------------------------------------------------------

    async fn exec(&self, c: &Cycle, a: &Action) -> Result<()> {
        debug!(pair = %self.pair.name, ?a, "ação");
        match a {
            Action::MoveLocal { from, to } => self.move_local(c, from, to).await,
            Action::MoveRemote { from, to } => self.move_remote(c, from, to).await,
            Action::CreateLocalDir { path } => self.create_local_dir(c, path),
            Action::CreateRemoteDir { path } => self.ensure_remote_dir(c, path).await.map(|_| ()),
            Action::Download { path } => self.with_active(path, self.download(c, path)).await,
            Action::Upload { path } => self.with_active(path, self.upload(c, path)).await,
            Action::Conflict { path } => self.with_active(path, self.conflict(c, path)).await,
            Action::RecordBase { path } => self.record_base(c, path),
            Action::DeleteLocal { path } => self.delete_local(c, path),
            Action::DeleteRemote { path } => self.delete_remote(c, path).await,
            Action::ForgetBase { path } => self.forget(c, path),
        }
    }

    async fn with_active<F: std::future::Future<Output = Result<()>>>(
        &self,
        path: &str,
        f: F,
    ) -> Result<()> {
        let p = path.to_string();
        self.set_status(|s| s.active.push(p.clone()));
        let r = f.await;
        self.set_status(|s| s.active.retain(|x| x != &p));
        r
    }

    fn put_base(&self, c: &Cycle, b: BaseEntry) -> Result<()> {
        self.st(|s| s.put_base(&b))?;
        c.base.lock().unwrap().insert(b.path.clone(), b);
        Ok(())
    }

    fn forget(&self, c: &Cycle, path: &str) -> Result<()> {
        self.st(|s| s.forget_base(path))?;
        c.base.lock().unwrap().retain(|p, _| p != path && !is_under(p, path));
        Ok(())
    }

    fn relocate(&self, c: &Cycle, from: &str, to: &str) -> Result<()> {
        self.st(|s| s.relocate_base(from, to))?;
        let mut base = c.base.lock().unwrap();
        let moved: Vec<RelPath> =
            base.keys().filter(|p| *p == from || is_under(p, from)).cloned().collect();
        for k in moved {
            let mut b = base.remove(&k).unwrap();
            b.path = format!("{to}{}", &k[from.len()..]);
            base.insert(b.path.clone(), b);
        }
        Ok(())
    }

    fn base_get(&self, c: &Cycle, path: &str) -> Option<BaseEntry> {
        c.base.lock().unwrap().get(path).cloned()
    }

    /// O arquivo local continua como estava na varredura? Protege contra
    /// sobrescrever/apagar algo que o usuário alterou durante o ciclo.
    fn local_unchanged(&self, c: &Cycle, path: &str) -> Result<bool> {
        let abs = self.abs(path);
        let md = match abs.symlink_metadata() {
            Ok(md) => Some(md),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(match (c.local.get(path), md) {
            (None, None) => true,
            (Some(l), Some(md)) => {
                l.is_dir == md.is_dir()
                    && (l.is_dir || (l.size == md.len() && l.mtime_ns == mtime_ns(&md)))
            }
            _ => false,
        })
    }

    fn create_local_dir(&self, c: &Cycle, path: &str) -> Result<()> {
        let abs = self.abs(path);
        std::fs::create_dir_all(&abs)?;
        let md = abs.metadata()?;
        let r = c.remote.get(path).context("pasta remota sumiu")?;
        self.put_base(
            c,
            BaseEntry {
                path: path.to_string(),
                is_dir: true,
                remote_id: r.item.id.clone(),
                remote_rev: r.item.rev.clone(),
                hash: None,
                size: 0,
                local_mtime_ns: mtime_ns(&md),
                inode: md.ino(),
            },
        )
    }

    /// Garante que a pasta remota `path` exista (criando ancestrais) e
    /// devolve o id. Idempotente dentro do ciclo.
    fn ensure_remote_dir<'a>(
        &'a self,
        c: &'a Cycle,
        path: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if path.is_empty() {
                return Ok(c.root_id.clone());
            }
            if let Some(b) = self.base_get(c, path)
                && b.is_dir
            {
                return Ok(b.remote_id);
            }
            let parent_id = self.ensure_remote_dir(c, parent_of(path).unwrap_or("")).await?;
            let item = self.provider.create_folder(&parent_id, name_of(path)).await?;
            self.st(|s| s.upsert_remote(std::slice::from_ref(&item)))?;
            let md = self.abs(path).metadata().ok();
            self.put_base(
                c,
                BaseEntry {
                    path: path.to_string(),
                    is_dir: true,
                    remote_id: item.id.clone(),
                    remote_rev: item.rev.clone(),
                    hash: None,
                    size: 0,
                    local_mtime_ns: md.as_ref().map(mtime_ns).unwrap_or(0),
                    inode: md.map(|m| m.ino()).unwrap_or(0),
                },
            )?;
            Ok(item.id)
        })
    }

    async fn move_local(&self, c: &Cycle, from: &str, to: &str) -> Result<()> {
        let (src, dst) = (self.abs(from), self.abs(to));
        if dst.symlink_metadata().is_ok() {
            bail!("destino já existe localmente");
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::rename(&src, &dst)?;
        self.relocate(c, from, to)?;
        if let Some(r) = c.remote.get(to)
            && let Some(mut b) = self.base_get(c, to)
        {
            b.remote_rev = r.item.rev.clone();
            self.put_base(c, b)?;
        }
        Ok(())
    }

    async fn move_remote(&self, c: &Cycle, from: &str, to: &str) -> Result<()> {
        let b = self.base_get(c, from).context("base ausente para mover")?;
        let parent_id = self.ensure_remote_dir(c, parent_of(to).unwrap_or("")).await?;
        // Atalho de documento nativo: no Drive o nome não tem a extensão.
        let native = c.remote.get(to).is_some_and(|r| r.item.native);
        let name = if native { crate::shortcut::strip_ext(name_of(to)) } else { name_of(to) };
        let item = self.provider.move_item(&b.remote_id, &parent_id, name).await?;
        self.st(|s| s.upsert_remote(std::slice::from_ref(&item)))?;
        self.relocate(c, from, to)?;
        if let Some(mut nb) = self.base_get(c, to) {
            nb.remote_rev = item.rev;
            if let Ok(md) = self.abs(to).symlink_metadata() {
                nb.inode = md.ino();
            }
            self.put_base(c, nb)?;
        }
        Ok(())
    }

    async fn download(&self, c: &Cycle, path: &str) -> Result<()> {
        let r = c.remote.get(path).context("item remoto sumiu")?;
        let dst = self.abs(path);
        let dir = dst.parent().context("sem pasta pai")?.to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".omarchy-drive-{}.tmp", tmp_suffix()));
        let res = async {
            self.provider.download(&r.item, &tmp).await?;
            let kind = self.provider.hash_kind();
            let t = tmp.clone();
            let got = tokio::task::spawn_blocking(move || hash_file(kind, &t)).await??;
            if let Some(expected) = &r.item.hash
                && *expected != got
            {
                bail!("hash do download não confere (esperado {expected}, obtido {got})");
            }
            if r.item.mtime_ms > 0 {
                let f = std::fs::File::options().write(true).open(&tmp)?;
                f.set_modified(UNIX_EPOCH + Duration::from_millis(r.item.mtime_ms as u64))?;
            }
            // Última verificação antes de substituir.
            if !self.local_unchanged(c, path)? {
                bail!("arquivo local mudou durante o download; será reavaliado");
            }
            std::fs::rename(&tmp, &dst)?;
            Ok(got)
        }
        .await;
        let got = match res {
            Ok(h) => h,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let md = dst.metadata()?;
        self.put_base(
            c,
            BaseEntry {
                path: path.to_string(),
                is_dir: false,
                remote_id: r.item.id.clone(),
                remote_rev: r.item.rev.clone(),
                hash: Some(got),
                size: md.len(),
                local_mtime_ns: mtime_ns(&md),
                inode: md.ino(),
            },
        )
    }

    async fn upload(&self, c: &Cycle, path: &str) -> Result<()> {
        let l = c.local.get(path).context("arquivo local sumiu")?;
        if !self.local_unchanged(c, path)? {
            bail!("arquivo local mudou desde a varredura; será reavaliado");
        }
        let parent_id = self.ensure_remote_dir(c, parent_of(path).unwrap_or("")).await?;
        let existing = c
            .remote
            .get(path)
            .filter(|r| !r.item.is_dir)
            .map(|r| r.item.id.clone())
            .or_else(|| self.base_get(c, path).filter(|b| !b.is_dir).map(|b| b.remote_id))
            // Se o remoto foi apagado, sobe como arquivo novo.
            .filter(|id| c.remote.values().any(|r| &r.item.id == id));
        let src = self.abs(path);
        let hash = match &l.hash {
            Some(h) => h.clone(),
            None => {
                let kind = self.provider.hash_kind();
                let s = src.clone();
                tokio::task::spawn_blocking(move || hash_file(kind, &s)).await??
            }
        };
        let item = self
            .provider
            .upload(&parent_id, name_of(path), &src, l.mtime_ns / 1_000_000, existing.as_deref())
            .await?;
        if let Some(rh) = &item.hash
            && *rh != hash
        {
            // O arquivo pode ter mudado durante o envio: o próximo ciclo sobe de novo.
            warn!(pair = %self.pair.name, path, "hash remoto difere do local após envio");
        }
        self.st(|s| s.upsert_remote(std::slice::from_ref(&item)))?;
        self.put_base(
            c,
            BaseEntry {
                path: path.to_string(),
                is_dir: false,
                remote_id: item.id.clone(),
                remote_rev: item.rev.clone(),
                hash: Some(hash),
                size: l.size,
                local_mtime_ns: l.mtime_ns,
                inode: l.inode,
            },
        )
    }

    /// Mantém as duas versões: a local vira uma cópia de conflito (que sobe
    /// no próximo ciclo) e a remota ocupa o nome original.
    async fn conflict(&self, c: &Cycle, path: &str) -> Result<()> {
        let src = self.abs(path);
        if !self.local_unchanged(c, path)? {
            bail!("arquivo local mudou durante o ciclo; será reavaliado");
        }
        let parent = src.parent().context("sem pasta pai")?;
        let renamed = parent.join(conflict_name(name_of(path)));
        std::fs::rename(&src, &renamed)
            .with_context(|| format!("renomeando para {}", renamed.display()))?;
        info!(pair = %self.pair.name, path, cópia = %renamed.display(), "conflito: mantidas as duas versões");
        self.forget(c, path)?;
        let r = c.remote.get(path).context("item remoto sumiu")?;
        // A versão local foi movida; para as verificações, o caminho agora
        // está vazio localmente.
        let c2 = Cycle {
            root_id: c.root_id.clone(),
            base: Mutex::new(c.base.lock().unwrap().clone()),
            local: c.local.iter().filter(|(p, _)| p.as_str() != path).map(|(k, v)| (k.clone(), v.clone())).collect(),
            remote: c.remote.clone(),
        };
        if r.item.is_dir {
            self.create_local_dir(&c2, path)?;
        } else {
            self.download(&c2, path).await?;
        }
        if let Some(b) = c2.base.lock().unwrap().get(path).cloned() {
            c.base.lock().unwrap().insert(path.to_string(), b);
        }
        Ok(())
    }

    fn record_base(&self, c: &Cycle, path: &str) -> Result<()> {
        let l = c.local.get(path).context("local ausente")?;
        let r = c.remote.get(path).context("remoto ausente")?;
        let old = self.base_get(c, path);
        self.put_base(
            c,
            BaseEntry {
                path: path.to_string(),
                is_dir: l.is_dir,
                remote_id: r.item.id.clone(),
                remote_rev: r.item.rev.clone(),
                hash: r.item.hash.clone().or_else(|| l.hash.clone()).or(old.and_then(|b| b.hash)),
                size: l.size,
                local_mtime_ns: l.mtime_ns,
                inode: l.inode,
            },
        )
    }

    fn delete_local(&self, c: &Cycle, path: &str) -> Result<()> {
        if !self.local_unchanged(c, path)? {
            bail!("arquivo local mudou; exclusão cancelada");
        }
        let abs = self.abs(path);
        if abs.symlink_metadata().is_ok() {
            self.move_to_trash(&abs)?;
        }
        self.forget(c, path)
    }

    fn move_to_trash(&self, abs: &Path) -> Result<()> {
        let res = match &self.trash_dir {
            Some(d) => crate::trash::trash(d, abs),
            None => Err(anyhow!("lixeira do sistema não encontrada")),
        };
        match res {
            Ok(()) => Ok(()),
            Err(e) => {
                // Outro sistema de arquivos (EXDEV) etc.: lixeira dentro da raiz,
                // ignorada pela sincronização.
                warn!("lixeira do sistema indisponível ({e:#}); usando .omarchy-drive-trash");
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let rel = abs.strip_prefix(&self.pair.local).unwrap_or(abs);
                let dst = self.pair.local.join(".omarchy-drive-trash").join(stamp.to_string()).join(rel);
                std::fs::create_dir_all(dst.parent().ok_or_else(|| anyhow!("sem pai"))?)?;
                std::fs::rename(abs, &dst)?;
                Ok(())
            }
        }
    }

    async fn delete_remote(&self, c: &Cycle, path: &str) -> Result<()> {
        let b = self.base_get(c, path).context("base ausente")?;
        // O remoto precisa ser exatamente o que conhecíamos; senão alguém
        // editou no meio do caminho e a exclusão não é segura.
        if let Some(r) = c.remote.get(path)
            && r.item.id != b.remote_id
        {
            bail!("item remoto foi substituído; exclusão cancelada");
        }
        self.provider.trash(&b.remote_id).await?;
        self.st(|s| s.remove_remote(std::slice::from_ref(&b.remote_id)))?;
        self.forget(c, path)
    }
}

/// Em pares somente-download, nada é enviado nem apagado no remoto.
fn restrict_download_only(plan: &mut Plan) {
    plan.actions.retain(|a| {
        !matches!(
            a,
            Action::Upload { .. }
                | Action::CreateRemoteDir { .. }
                | Action::MoveRemote { .. }
                | Action::DeleteRemote { .. }
        )
    });
}
