//! Testes ponta a ponta do motor com um provedor em memória.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use async_trait::async_trait;
use omarchy_drive_core::config::{Settings, SyncPair};
use omarchy_drive_core::engine::PairEngine;
use omarchy_drive_core::hash::hash_bytes;
use omarchy_drive_core::state::State;
use omarchy_drive_core::oauth::{AuthError, is_auth_expired};
use omarchy_drive_core::status::{PairState, StatusHub};
use omarchy_drive_core::{ChangeBatch, HashKind, Provider, Quota, RemoteItem, shortcut};

#[derive(Default)]
struct Inner {
    items: HashMap<String, (RemoteItem, Vec<u8>)>,
    log: Vec<String>,
    next_id: u64,
}

#[derive(Default)]
struct MemProvider {
    inner: Mutex<Inner>,
    downloads: AtomicUsize,
    uploads: AtomicUsize,
    /// Simula o provedor recusando o refresh token.
    auth_expired: std::sync::atomic::AtomicBool,
}

impl MemProvider {
    fn new() -> Arc<Self> {
        let p = Self::default();
        p.inner.lock().unwrap().items.insert(
            "root".into(),
            (
                RemoteItem {
                    id: "root".into(),
                    parent_id: None,
                    name: "".into(),
                    is_dir: true,
                    size: 0,
                    mtime_ms: 0,
                    rev: "0".into(),
                    hash: None,
                    native: false,
                },
                vec![],
            ),
        );
        Arc::new(p)
    }

    fn touch(inner: &mut Inner, id: &str) {
        inner.log.push(id.to_string());
    }

    fn find(inner: &Inner, path: &str) -> Option<String> {
        let mut cur = "root".to_string();
        for seg in path.split('/') {
            cur = inner
                .items
                .values()
                .find(|(i, _)| i.parent_id.as_deref() == Some(&cur) && i.name == seg)?
                .0
                .id
                .clone();
        }
        Some(cur)
    }

    fn new_item(inner: &mut Inner, parent: &str, name: &str, is_dir: bool, data: &[u8]) -> RemoteItem {
        inner.next_id += 1;
        let id = format!("id{}", inner.next_id);
        let item = RemoteItem {
            id: id.clone(),
            parent_id: Some(parent.into()),
            name: name.into(),
            is_dir,
            size: data.len() as u64,
            mtime_ms: 1_600_000_000_000 + inner.next_id as i64 * 1000,
            rev: "1".into(),
            hash: (!is_dir).then(|| hash_bytes(HashKind::Md5, data)),
            native: false,
        };
        inner.items.insert(id.clone(), (item.clone(), data.to_vec()));
        Self::touch(inner, &id);
        item
    }

    /// Simula outro dispositivo gravando um arquivo no drive.
    fn remote_write(&self, path: &str, data: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        let mut parent = "root".to_string();
        let segs: Vec<&str> = path.split('/').collect();
        for (i, seg) in segs.iter().enumerate() {
            let partial = segs[..=i].join("/");
            let last = i == segs.len() - 1;
            match Self::find(&g, &partial) {
                Some(id) if last => {
                    let (it, d) = g.items.get_mut(&id).unwrap();
                    *d = data.to_vec();
                    it.size = data.len() as u64;
                    it.hash = Some(hash_bytes(HashKind::Md5, data));
                    it.rev = (it.rev.parse::<u64>().unwrap() + 1).to_string();
                    it.mtime_ms += 1000;
                    Self::touch(&mut g, &id);
                }
                Some(id) => parent = id,
                None => parent = Self::new_item(&mut g, &parent, seg, !last, if last { data } else { b"" }).id,
            }
        }
    }

    /// Cria um Google Doc nativo (já mapeado: nome com `.gdoc`, hash do atalho).
    fn remote_native_doc(&self, name: &str) {
        let mut g = self.inner.lock().unwrap();
        let file = format!("{name}.gdoc");
        let mut it = Self::new_item(&mut g, "root", &file, false, b"");
        let bytes = shortcut::content(&it.id, &file);
        it.native = true;
        it.size = bytes.len() as u64;
        it.hash = Some(hash_bytes(HashKind::Md5, &bytes));
        g.items.insert(it.id.clone(), (it.clone(), Vec::new()));
        Self::touch(&mut g, &it.id);
    }

    fn remote_rename(&self, path: &str, new_name: &str) {
        let mut g = self.inner.lock().unwrap();
        let id = Self::find(&g, path).unwrap();
        let it = &mut g.items.get_mut(&id).unwrap().0;
        it.name = new_name.into();
        // Como o Google Drive: renomear incrementa a versão.
        it.rev = (it.rev.parse::<u64>().unwrap() + 1).to_string();
        Self::touch(&mut g, &id);
    }

    fn remote_read(&self, path: &str) -> Option<Vec<u8>> {
        let g = self.inner.lock().unwrap();
        Self::find(&g, path).map(|id| g.items[&id].1.clone())
    }

    fn remote_names(&self) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<String> = g.items.values().filter(|(i, _)| i.id != "root").map(|(i, _)| i.name.clone()).collect();
        v.sort();
        v
    }
}

#[async_trait]
impl Provider for MemProvider {
    fn name(&self) -> &'static str {
        "mem"
    }
    fn hash_kind(&self) -> HashKind {
        HashKind::Md5
    }
    async fn resolve_folder(&self, path: &str) -> Result<RemoteItem> {
        assert_eq!(path, "");
        Ok(self.inner.lock().unwrap().items["root"].0.clone())
    }
    async fn start_cursor(&self, _root: &str) -> Result<String> {
        Ok(self.inner.lock().unwrap().log.len().to_string())
    }
    async fn list_tree(&self, root: &str) -> Result<Vec<RemoteItem>> {
        // Descendentes de `root` (a raiz do par ou uma subpasta).
        let g = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let mut stack = vec![root.to_string()];
        while let Some(cur) = stack.pop() {
            for (i, _) in g.items.values().filter(|(i, _)| i.parent_id.as_deref() == Some(&cur)) {
                stack.push(i.id.clone());
                out.push(i.clone());
            }
        }
        Ok(out)
    }
    async fn changes(&self, _root: &str, cursor: &str) -> Result<ChangeBatch> {
        if self.auth_expired.load(Ordering::SeqCst) {
            return Err(anyhow::Error::from(AuthError::Expired("Token has been expired or revoked.".into()))
                .context("listando mudanças"));
        }
        let g = self.inner.lock().unwrap();
        let from: usize = cursor.parse()?;
        let mut b = ChangeBatch { cursor: g.log.len().to_string(), ..Default::default() };
        for id in &g.log[from..] {
            match g.items.get(id) {
                Some((i, _)) => b.upserts.push(i.clone()),
                None => b.removed.push(id.clone()),
            }
        }
        Ok(b)
    }
    async fn download(&self, item: &RemoteItem, dest: &Path) -> Result<()> {
        if item.native {
            // Como o provedor do Google: atalho gerado localmente, sem rede.
            std::fs::write(dest, shortcut::content(&item.id, &item.name))?;
            return Ok(());
        }
        self.downloads.fetch_add(1, Ordering::SeqCst);
        let data = self.inner.lock().unwrap().items[&item.id].1.clone();
        std::fs::write(dest, data)?;
        Ok(())
    }
    async fn upload(&self, parent: &str, name: &str, src: &Path, mtime_ms: i64, existing: Option<&str>) -> Result<RemoteItem> {
        self.uploads.fetch_add(1, Ordering::SeqCst);
        let data = std::fs::read(src)?;
        let mut g = self.inner.lock().unwrap();
        let item = match existing {
            Some(id) => {
                let (it, d) = g.items.get_mut(id).unwrap();
                *d = data.clone();
                it.size = data.len() as u64;
                it.hash = Some(hash_bytes(HashKind::Md5, &data));
                it.rev = (it.rev.parse::<u64>().unwrap() + 1).to_string();
                it.mtime_ms = mtime_ms;
                let it = it.clone();
                Self::touch(&mut g, id);
                it
            }
            None => {
                let mut it = Self::new_item(&mut g, parent, name, false, &data);
                it.mtime_ms = mtime_ms;
                g.items.get_mut(&it.id).unwrap().0.mtime_ms = mtime_ms;
                it
            }
        };
        Ok(item)
    }
    async fn create_folder(&self, parent: &str, name: &str) -> Result<RemoteItem> {
        let mut g = self.inner.lock().unwrap();
        Ok(Self::new_item(&mut g, parent, name, true, b""))
    }
    async fn move_item(&self, id: &str, parent: &str, name: &str) -> Result<RemoteItem> {
        let mut g = self.inner.lock().unwrap();
        let Some((it, _)) = g.items.get_mut(id) else { bail!("404") };
        it.parent_id = Some(parent.into());
        if it.native {
            // O Drive guarda o nome sem extensão; o mapeamento devolve com ".gdoc".
            assert!(!shortcut::is_shortcut_path(name), "nome com extensão enviado ao Drive: {name}");
            it.name = format!("{name}.gdoc");
        } else {
            it.name = name.into();
        }
        let it = it.clone();
        Self::touch(&mut g, id);
        Ok(it)
    }
    async fn trash(&self, id: &str) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        // Remove o item e a subárvore.
        let mut stack = vec![id.to_string()];
        while let Some(cur) = stack.pop() {
            let kids: Vec<String> = g.items.values().filter(|(i, _)| i.parent_id.as_deref() == Some(&cur)).map(|(i, _)| i.id.clone()).collect();
            stack.extend(kids);
            g.items.remove(&cur);
            Self::touch(&mut g, &cur);
        }
        Ok(())
    }
    async fn quota(&self) -> Result<Quota> {
        Ok(Quota { used: 0, total: None })
    }
}

// ---------------------------------------------------------------------------

static MTIME: AtomicI64 = AtomicI64::new(0);

/// Grava com mtime no passado (o scanner ignora arquivos "ainda sendo escritos").
fn write_local(root: &Path, rel: &str, data: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, data).unwrap();
    let n = MTIME.fetch_add(1, Ordering::SeqCst);
    let t = SystemTime::now() - Duration::from_secs(3600) + Duration::from_secs(n as u64);
    std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
}

fn read_local(root: &Path, rel: &str) -> Option<Vec<u8>> {
    std::fs::read(root.join(rel)).ok()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    mem: Arc<MemProvider>,
    engine: PairEngine,
    status: Arc<StatusHub>,
}

fn env(max_deletes: usize) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Drive");
    std::fs::create_dir_all(&root).unwrap();
    let mem = MemProvider::new();
    let pair = SyncPair {
        name: "t".into(),
        account: "a".into(),
        local: root.clone(),
        remote: "".into(),
        include: vec![],
        ignore: vec![],
        download_only: false,
        full_scan_only: false,
        paused: false,
    };
    let settings = Settings { max_deletes_without_confirm: max_deletes, ..Default::default() };
    let status = Arc::new(StatusHub::new(None));
    let engine = PairEngine::new(
        pair,
        mem.clone(),
        State::open(&dir.path().join("state.sqlite")).unwrap(),
        settings,
        status.clone(),
    )
    .unwrap()
    .with_trash_dir(dir.path().join("Trash"));
    Env { _dir: dir, root, mem, engine, status }
}

impl Env {
    async fn sync(&self) -> omarchy_drive_core::engine::CycleReport {
        let r = self.engine.sync_once(false).await.unwrap();
        assert_eq!(r.failed, 0, "ações falharam: {r:?}");
        r
    }
    /// Sincroniza até estabilizar (conflitos pedem um ciclo extra).
    async fn settle(&self) {
        for _ in 0..4 {
            let r = self.sync().await;
            if r.executed == 0 {
                return;
            }
        }
        panic!("não estabilizou");
    }
}

#[tokio::test]
async fn initial_sync_merges_both_sides_and_is_idempotent() {
    let e = env(50);
    write_local(&e.root, "local.txt", b"L");
    write_local(&e.root, "Pasta/sub/x.txt", b"X");
    e.mem.remote_write("remoto.txt", b"R");
    e.mem.remote_write("Docs/a.md", b"A");

    e.settle().await;
    assert_eq!(read_local(&e.root, "remoto.txt").unwrap(), b"R");
    assert_eq!(read_local(&e.root, "Docs/a.md").unwrap(), b"A");
    assert_eq!(e.mem.remote_read("local.txt").unwrap(), b"L");
    assert_eq!(e.mem.remote_read("Pasta/sub/x.txt").unwrap(), b"X");

    let r = e.sync().await;
    assert_eq!(r.executed, 0, "segundo ciclo deveria ser vazio");
}

#[tokio::test]
async fn edits_propagate_in_both_directions() {
    let e = env(50);
    write_local(&e.root, "a.txt", b"v1");
    e.mem.remote_write("b.txt", b"v1");
    e.settle().await;

    write_local(&e.root, "a.txt", b"v2 local");
    e.mem.remote_write("b.txt", b"v2 remoto");
    e.settle().await;
    assert_eq!(e.mem.remote_read("a.txt").unwrap(), b"v2 local");
    assert_eq!(read_local(&e.root, "b.txt").unwrap(), b"v2 remoto");
}

#[tokio::test]
async fn conflict_keeps_both_versions_everywhere() {
    let e = env(50);
    write_local(&e.root, "doc.txt", b"base");
    e.settle().await;

    write_local(&e.root, "doc.txt", b"edicao local");
    e.mem.remote_write("doc.txt", b"edicao remota");
    e.settle().await;

    assert_eq!(read_local(&e.root, "doc.txt").unwrap(), b"edicao remota");
    let copies: Vec<_> = std::fs::read_dir(&e.root)
        .unwrap()
        .flatten()
        .map(|d| d.file_name().into_string().unwrap())
        .filter(|n| n.contains("conflito"))
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(read_local(&e.root, &copies[0]).unwrap(), b"edicao local");
    assert_eq!(e.mem.remote_read(&copies[0]).unwrap(), b"edicao local");
}

#[tokio::test]
async fn remote_rename_is_applied_without_redownload() {
    let e = env(50);
    e.mem.remote_write("Fotos/praia.jpg", b"jpeg");
    e.settle().await;
    let before = e.mem.downloads.load(Ordering::SeqCst);

    e.mem.remote_rename("Fotos", "Viagem");
    e.settle().await;
    assert_eq!(read_local(&e.root, "Viagem/praia.jpg").unwrap(), b"jpeg");
    assert!(!e.root.join("Fotos").exists());
    assert_eq!(e.mem.downloads.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn remote_file_rename_with_rev_bump_and_local_edit() {
    let e = env(50);
    e.mem.remote_write("a.txt", b"v1");
    e.mem.remote_write("b.txt", b"v1");
    e.settle().await;

    e.mem.remote_rename("a.txt", "a2.txt");
    e.mem.remote_rename("b.txt", "b2.txt");
    write_local(&e.root, "b.txt", b"v2 local");
    e.settle().await;
    assert_eq!(read_local(&e.root, "a2.txt").unwrap(), b"v1");
    assert_eq!(e.mem.remote_read("b2.txt").unwrap(), b"v2 local");
    assert_eq!(read_local(&e.root, "b2.txt").unwrap(), b"v2 local");
}

#[tokio::test]
async fn local_rename_is_applied_without_reupload() {
    let e = env(50);
    write_local(&e.root, "Projetos/plano.txt", b"plano");
    e.settle().await;
    let before = e.mem.uploads.load(Ordering::SeqCst);

    std::fs::rename(e.root.join("Projetos"), e.root.join("Arquivo")).unwrap();
    e.settle().await;
    assert_eq!(e.mem.remote_read("Arquivo/plano.txt").unwrap(), b"plano");
    assert!(e.mem.remote_read("Projetos").is_none());
    assert_eq!(e.mem.uploads.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn deletes_propagate_to_trash() {
    let e = env(50);
    write_local(&e.root, "apagar-local.txt", b"1");
    e.mem.remote_write("apagar-remoto.txt", b"2");
    e.settle().await;

    std::fs::remove_file(e.root.join("apagar-local.txt")).unwrap();
    e.mem.trash(&{
        let g = e.mem.inner.lock().unwrap();
        MemProvider::find(&g, "apagar-remoto.txt").unwrap()
    })
    .await
    .unwrap();
    e.settle().await;
    assert!(e.mem.remote_read("apagar-local.txt").is_none());
    assert!(!e.root.join("apagar-remoto.txt").exists());
}

#[tokio::test]
async fn mass_delete_is_held_until_confirmed() {
    let e = env(3);
    for i in 0..10 {
        write_local(&e.root, &format!("f{i}.txt"), b"x");
    }
    e.settle().await;
    for i in 0..10 {
        std::fs::remove_file(e.root.join(format!("f{i}.txt"))).unwrap();
    }
    let r = e.sync().await;
    assert_eq!(r.held_deletes, 10);
    assert_eq!(e.mem.remote_names().len(), 10, "nada pode ter sido apagado");

    let r = e.engine.sync_once(true).await.unwrap();
    assert_eq!(r.executed, 10);
    assert!(e.mem.remote_names().is_empty());
}

#[tokio::test]
async fn missing_local_root_never_deletes_remote() {
    let e = env(50);
    write_local(&e.root, "importante.txt", b"!");
    e.settle().await;
    std::fs::rename(&e.root, e.root.with_file_name("Drive-desmontado")).unwrap();
    assert!(e.engine.sync_once(true).await.is_err());
    assert_eq!(e.mem.remote_read("importante.txt").unwrap(), b"!");
}

#[tokio::test]
async fn login_expirado_deixa_o_par_em_auth_expired() {
    let e = env(50);
    write_local(&e.root, "a.txt", b"A");
    e.settle().await;

    e.mem.auth_expired.store(true, Ordering::SeqCst);
    let err = e.engine.sync_once(false).await.unwrap_err();
    assert!(is_auth_expired(&err), "{err:#}");
    let st = e.status.snapshot().pairs["t"].clone();
    assert_eq!(st.state, PairState::AuthExpired);
    assert!(st.last_error.unwrap().contains("login expirado"));
    // Nada foi apagado ou alterado por causa da falha.
    assert_eq!(read_local(&e.root, "a.txt").unwrap(), b"A");

    // Depois de reautorizar, o ciclo volta ao normal.
    e.mem.auth_expired.store(false, Ordering::SeqCst);
    e.sync().await;
    assert_eq!(e.status.snapshot().pairs["t"].state, PairState::Idle);
}

#[tokio::test]
async fn documento_nativo_vira_atalho_sem_download() {
    let e = env(50);
    e.mem.remote_native_doc("Relatório");
    e.settle().await;
    let local = read_local(&e.root, "Relatório.gdoc").expect("atalho criado");
    assert!(shortcut::url_from(&local).unwrap().starts_with("https://docs.google.com/document/d/"));
    assert_eq!(e.mem.downloads.load(Ordering::SeqCst), 0, "documento nativo não pode ser baixado");

    // Edição local do atalho: nada sobe, o conteúdo é restaurado.
    write_local(&e.root, "Relatório.gdoc", b"editado");
    e.settle().await;
    assert_eq!(read_local(&e.root, "Relatório.gdoc").unwrap(), local);
    assert_eq!(e.mem.uploads.load(Ordering::SeqCst), 0);

    // Rename local renomeia o documento (sem a extensão no Drive).
    std::fs::rename(e.root.join("Relatório.gdoc"), e.root.join("Final.gdoc")).unwrap();
    e.settle().await;
    assert_eq!(e.mem.remote_names(), vec!["Final.gdoc".to_string()]);

    // Exclusão local manda o documento para a lixeira.
    std::fs::remove_file(e.root.join("Final.gdoc")).unwrap();
    e.settle().await;
    assert!(e.mem.remote_names().is_empty());
    assert_eq!(e.mem.uploads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn atalho_copiado_de_fora_nao_sobe() {
    let e = env(50);
    write_local(&e.root, "Alheio.gdoc", &shortcut::content("ID-DE-OUTRA-CONTA", "Alheio.gdoc"));
    e.settle().await;
    assert_eq!(e.mem.uploads.load(Ordering::SeqCst), 0);
    assert!(e.status.snapshot().pairs["t"].warnings.iter().any(|w| w.contains("Alheio.gdoc")));
}

#[tokio::test]
async fn varredura_incremental_usa_so_os_caminhos_sujos() {
    use omarchy_drive_core::engine::ScanScope;
    use std::collections::HashSet;
    let paths = |v: &[&str]| ScanScope::Paths(v.iter().map(|s| s.to_string()).collect::<HashSet<_>>());
    let e = env(50);
    write_local(&e.root, "a.txt", b"A");
    e.settle().await; // primeira varredura: completa

    // Ciclo só do remoto não lê o disco: arquivo novo sem evento fica para depois.
    write_local(&e.root, "sem-evento.txt", b"S");
    let r = e.engine.sync_scoped(false, paths(&[])).await.unwrap();
    assert_eq!(r.executed, 0);
    assert!(e.mem.remote_read("sem-evento.txt").is_none());

    // Com o evento, sobe.
    let r = e.engine.sync_scoped(false, paths(&["sem-evento.txt"])).await.unwrap();
    assert_eq!(r.executed, 1);
    assert_eq!(e.mem.remote_read("sem-evento.txt").unwrap(), b"S");

    // Mudança remota baixada; o motor marca o caminho e o ciclo seguinte fica ocioso.
    e.mem.remote_write("Docs/r.md", b"R");
    let r = e.engine.sync_scoped(false, paths(&[])).await.unwrap();
    assert!(r.executed >= 1);
    assert_eq!(read_local(&e.root, "Docs/r.md").unwrap(), b"R");
    let r = e.engine.sync_scoped(false, paths(&[])).await.unwrap();
    assert_eq!(r.executed, 0, "{r:?}");

    // Exclusão local com evento propaga; sem confirmação no disco não propagaria.
    std::fs::remove_file(e.root.join("a.txt")).unwrap();
    e.engine.sync_scoped(false, paths(&["a.txt"])).await.unwrap();
    assert!(e.mem.remote_read("a.txt").is_none());

    // Uma completa depois de tudo não encontra diferença.
    assert_eq!(e.engine.sync_once(false).await.unwrap().executed, 0);
}
