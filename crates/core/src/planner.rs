//! Planejador de sincronização de 3 vias.
//!
//! Compara, caminho a caminho, o estado **base** (último estado sincronizado),
//! o estado **local** (varredura do disco) e o estado **remoto** (árvore do
//! provedor) e produz uma lista ordenada de [`Action`]s. É uma função pura:
//! não faz I/O, o que permite testar todas as regras de decisão.
//!
//! Princípios (os mesmos dos clientes pagos):
//! - nunca perder dados: em caso de dúvida, conflito = manter as duas cópias;
//! - edição vence exclusão: um lado apagou e o outro editou → restaura;
//! - movimentações são detectadas (id remoto / inode local) e aplicadas como
//!   rename, sem baixar/subir o conteúdo de novo;
//! - pastas apagadas viram uma única ação na pasta mais alta.

use std::collections::{BTreeMap, HashMap};

use crate::model::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Aplica localmente um rename/move feito no remoto.
    MoveLocal { from: RelPath, to: RelPath },
    /// Aplica no remoto um rename/move feito localmente.
    MoveRemote { from: RelPath, to: RelPath },
    CreateLocalDir { path: RelPath },
    CreateRemoteDir { path: RelPath },
    /// Remoto → local (arquivo novo ou modificado).
    Download { path: RelPath },
    /// Local → remoto (arquivo novo ou modificado).
    Upload { path: RelPath },
    /// Os dois lados mudaram de formas diferentes: a cópia local é renomeada
    /// para um nome de conflito (e enviada), e a remota é baixada no lugar.
    Conflict { path: RelPath },
    /// Os dois lados já estão iguais: apenas registra a base.
    RecordBase { path: RelPath },
    /// Envia para a lixeira local.
    DeleteLocal { path: RelPath },
    /// Envia para a lixeira do provedor.
    DeleteRemote { path: RelPath },
    /// Sumiu dos dois lados: esquece a base.
    ForgetBase { path: RelPath },
}

impl Action {
    pub fn path(&self) -> &str {
        match self {
            Action::MoveLocal { to, .. } | Action::MoveRemote { to, .. } => to,
            Action::CreateLocalDir { path }
            | Action::CreateRemoteDir { path }
            | Action::Download { path }
            | Action::Upload { path }
            | Action::Conflict { path }
            | Action::RecordBase { path }
            | Action::DeleteLocal { path }
            | Action::DeleteRemote { path }
            | Action::ForgetBase { path } => path,
        }
    }

    fn phase(&self) -> u8 {
        match self {
            Action::MoveLocal { .. } | Action::MoveRemote { .. } => 0,
            Action::CreateLocalDir { .. } | Action::CreateRemoteDir { .. } => 1,
            Action::Download { .. }
            | Action::Upload { .. }
            | Action::Conflict { .. }
            | Action::RecordBase { .. } => 2,
            Action::DeleteLocal { .. } | Action::DeleteRemote { .. } => 3,
            Action::ForgetBase { .. } => 4,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// Estados local e remoto já com os moves detectados aplicados: as ações
    /// após um move referem-se aos caminhos novos, então o executor deve usar
    /// estes mapas, não os da entrada.
    pub local: HashMap<RelPath, LocalEntry>,
    pub remote: HashMap<RelPath, RemoteEntry>,
    /// Avisos para o status do par (ex.: atalho local sem documento remoto).
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
    pub fn local_deletes(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a, Action::DeleteLocal { .. })).count()
    }
    pub fn remote_deletes(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a, Action::DeleteRemote { .. })).count()
    }
}

/// Mapas de entrada, indexados por caminho relativo.
pub struct Snapshot {
    pub base: HashMap<RelPath, BaseEntry>,
    pub local: HashMap<RelPath, LocalEntry>,
    pub remote: HashMap<RelPath, RemoteEntry>,
}

fn local_changed(b: &BaseEntry, l: &LocalEntry) -> bool {
    if l.is_dir != b.is_dir {
        return true;
    }
    if l.is_dir {
        return false;
    }
    if l.size == b.size && l.mtime_ns == b.local_mtime_ns {
        return false;
    }
    match &l.hash {
        Some(h) => b.hash.as_ref() != Some(h),
        None => true,
    }
}

fn remote_changed(b: &BaseEntry, r: &RemoteEntry) -> bool {
    if r.item.is_dir != b.is_dir {
        return true;
    }
    if r.item.is_dir {
        return false;
    }
    if r.item.id != b.remote_id {
        return true;
    }
    match (&r.item.hash, &b.hash) {
        (Some(rh), Some(bh)) => rh != bh,
        _ => r.item.rev != b.remote_rev,
    }
}

fn same_content(l: &LocalEntry, r: &RemoteEntry) -> bool {
    match (&l.hash, &r.item.hash) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Renomeia `from` (e tudo abaixo dele) para `to` dentro de um mapa.
fn relocate<T>(
    map: &mut BTreeMap<RelPath, T>,
    from: &str,
    to: &str,
    mut fix: impl FnMut(&mut T, &str),
) {
    let keys: Vec<RelPath> = map
        .range(from.to_string()..)
        .take_while(|(k, _)| k.starts_with(from))
        .filter(|(k, _)| k.as_str() == from || is_under(k, from))
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys {
        let mut v = map.remove(&k).unwrap();
        let nk = format!("{to}{}", &k[from.len()..]);
        fix(&mut v, &nk);
        map.insert(nk, v);
    }
}

pub fn plan(snap: &Snapshot) -> Plan {
    plan_checked(snap, |_| Ok(()))
}

/// Como [`plan`], recusando o que o provedor não aceita: `check` recebe o
/// caminho relativo ao par e devolve o motivo da recusa.
pub fn plan_checked(snap: &Snapshot, check: impl Fn(&str) -> Result<(), String>) -> Plan {
    let mut base: BTreeMap<RelPath, BaseEntry> =
        snap.base.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut local: BTreeMap<RelPath, LocalEntry> =
        snap.local.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut remote: BTreeMap<RelPath, RemoteEntry> =
        snap.remote.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    let mut actions: Vec<Action> = Vec::new();

    // ---- 1. Movimentações remotas (mesmo id, caminho diferente) ----
    let remote_by_id: HashMap<String, RelPath> =
        remote.iter().map(|(p, r)| (r.item.id.clone(), p.clone())).collect();
    let mut ids: Vec<(usize, String)> =
        base.values().map(|b| (depth(&b.path), b.remote_id.clone())).collect();
    ids.sort();
    let mut base_by_id: HashMap<String, RelPath> =
        base.iter().map(|(p, b)| (b.remote_id.clone(), p.clone())).collect();
    for (_, id) in &ids {
        let Some(cur) = base_by_id.get(id).cloned() else { continue };
        let Some(target) = remote_by_id.get(id) else { continue };
        if *target == cur
            || base.contains_key(target)
            || local.contains_key(target)
            || !local.get(&cur).is_some_and(|l| l.is_dir == base[&cur].is_dir)
        {
            continue;
        }
        actions.push(Action::MoveLocal { from: cur.clone(), to: target.clone() });
        relocate(&mut base, &cur, target, |b, p| {
            b.path = p.to_string();
            base_by_id.insert(b.remote_id.clone(), p.to_string());
        });
        relocate(&mut local, &cur, target, |l, p| l.path = p.to_string());
    }

    // ---- 2. Movimentações locais (mesmo inode, caminho diferente) ----
    let local_new_by_inode: HashMap<u64, RelPath> = local
        .iter()
        .filter(|(p, l)| l.inode != 0 && !base.contains_key(*p))
        .map(|(p, l)| (l.inode, p.clone()))
        .collect();
    let mut paths: Vec<RelPath> = base.keys().cloned().collect();
    paths.sort_by_key(|p| depth(p));
    for orig in paths {
        // A entrada pode já ter sido realocada junto com a pasta pai.
        let Some(b) = base.get(&orig).cloned() else { continue };
        if local.contains_key(&orig) || b.inode == 0 {
            continue;
        }
        let Some(target) = local_new_by_inode.get(&b.inode) else { continue };
        if base.contains_key(target) || remote.contains_key(target) {
            continue;
        }
        let Some(l) = local.get(target) else { continue };
        if l.is_dir != b.is_dir {
            continue;
        }
        // O remoto precisa continuar no lugar antigo, com o mesmo id.
        if !remote.get(&orig).is_some_and(|r| r.item.id == b.remote_id) {
            continue;
        }
        // Proteção contra reuso de inode: arquivo precisa ter o mesmo conteúdo
        // (ou os mesmos metadados).
        if !b.is_dir {
            let same_meta = l.size == b.size && l.mtime_ns == b.local_mtime_ns;
            let same_hash = l.hash.is_some() && l.hash == b.hash;
            if !same_meta && !same_hash {
                continue;
            }
        }
        actions.push(Action::MoveRemote { from: orig.clone(), to: target.clone() });
        relocate(&mut base, &orig, target, |b, p| b.path = p.to_string());
        relocate(&mut remote, &orig, target, |r, p| r.path = p.to_string());
    }

    // ---- 3. Decisão caminho a caminho ----
    let mut all: Vec<&RelPath> = base.keys().chain(local.keys()).chain(remote.keys()).collect();
    all.sort();
    all.dedup();

    let mut per_path: Vec<Action> = Vec::new();
    for path in all {
        let p = path.clone();
        let b = base.get(path);
        let l = local.get(path);
        let r = remote.get(path);
        let act = match (b, l, r) {
            (None, None, None) => None,
            (None, Some(l), None) => Some(if l.is_dir {
                Action::CreateRemoteDir { path: p }
            } else {
                Action::Upload { path: p }
            }),
            (None, None, Some(r)) => Some(if r.item.is_dir {
                Action::CreateLocalDir { path: p }
            } else {
                Action::Download { path: p }
            }),
            (None, Some(l), Some(r)) => Some(match (l.is_dir, r.item.is_dir) {
                (true, true) => Action::RecordBase { path: p },
                (false, false) if same_content(l, r) => Action::RecordBase { path: p },
                _ => Action::Conflict { path: p },
            }),
            (Some(_), None, None) => Some(Action::ForgetBase { path: p }),
            (Some(b), Some(l), Some(r)) => {
                let ld = local_changed(b, l);
                let rd = remote_changed(b, r);
                if l.is_dir && r.item.is_dir {
                    (r.item.id != b.remote_id || l.inode != b.inode)
                        .then_some(Action::RecordBase { path: p })
                } else {
                    match (ld, rd) {
                        (false, false) => {
                            let stale = l.mtime_ns != b.local_mtime_ns
                                || l.inode != b.inode
                                || r.item.rev != b.remote_rev;
                            stale.then_some(Action::RecordBase { path: p })
                        }
                        (true, false) => Some(Action::Upload { path: p }),
                        (false, true) => Some(Action::Download { path: p }),
                        (true, true) => Some(
                            if l.is_dir == r.item.is_dir && same_content(l, r) {
                                Action::RecordBase { path: p }
                            } else {
                                Action::Conflict { path: p }
                            },
                        ),
                    }
                }
            }
            // Apagado localmente.
            (Some(b), None, Some(r)) => Some(if remote_changed(b, r) {
                if r.item.is_dir {
                    Action::CreateLocalDir { path: p }
                } else {
                    Action::Download { path: p }
                }
            } else {
                Action::DeleteRemote { path: p }
            }),
            // Apagado no remoto.
            (Some(b), Some(l), None) => Some(if local_changed(b, l) {
                if l.is_dir {
                    Action::CreateRemoteDir { path: p }
                } else {
                    Action::Upload { path: p }
                }
            } else {
                Action::DeleteLocal { path: p }
            }),
        };
        if let Some(a) = act {
            per_path.push(a);
        }
    }

    // ---- 3b. Atalhos de documentos nativos ----
    // O conteúdo do atalho pertence ao remoto: edição local nunca sobe e é
    // restaurada; atalho sem documento remoto nunca vira upload.
    let mut warnings = Vec::new();
    per_path.retain_mut(|a| {
        let path = a.path().to_string();
        let native_remote = remote.get(&path).is_some_and(|r| r.item.native);
        match a {
            Action::Upload { .. } | Action::Conflict { .. } if native_remote => {
                *a = Action::Download { path };
                true
            }
            Action::Upload { .. } if crate::shortcut::is_shortcut_path(&path) && !remote.contains_key(&path) => {
                if base.contains_key(&path) {
                    // O documento foi apagado no Drive: o atalho editado vai junto.
                    *a = Action::DeleteLocal { path };
                    true
                } else {
                    warnings.push(format!("atalho sem documento no Drive, ignorado: {path}"));
                    false
                }
            }
            _ => true,
        }
    });

    // ---- 4. Consolidação de exclusões de pastas ----
    // Uma pasta apagada de um lado só é apagada do outro se nada abaixo dela
    // precisar ser mantido; senão ela é recriada. Exclusões abaixo de uma
    // pasta que será apagada são absorvidas por ela.
    let dir_deletes: Vec<(RelPath, bool)> = per_path
        .iter()
        .filter_map(|a| match a {
            Action::DeleteRemote { path } if base.get(path).is_some_and(|b| b.is_dir) => {
                Some((path.clone(), true))
            }
            Action::DeleteLocal { path } if base.get(path).is_some_and(|b| b.is_dir) => {
                Some((path.clone(), false))
            }
            _ => None,
        })
        .collect();
    for (dir, remote_side) in &dir_deletes {
        let keeps = per_path.iter().any(|a| {
            is_under(a.path(), dir)
                && if *remote_side {
                    matches!(
                        a,
                        Action::Download { .. }
                            | Action::CreateLocalDir { .. }
                            | Action::Conflict { .. }
                    )
                } else {
                    matches!(
                        a,
                        Action::Upload { .. }
                            | Action::CreateRemoteDir { .. }
                            | Action::Conflict { .. }
                    )
                }
        });
        for a in per_path.iter_mut() {
            if a.path() == dir && keeps {
                *a = if *remote_side {
                    Action::CreateLocalDir { path: dir.clone() }
                } else {
                    Action::CreateRemoteDir { path: dir.clone() }
                };
            }
        }
        if !keeps {
            per_path.retain(|a| {
                !(is_under(a.path(), dir)
                    && matches!(
                        a,
                        Action::DeleteRemote { .. }
                            | Action::DeleteLocal { .. }
                            | Action::ForgetBase { .. }
                    ))
            });
        }
    }

    // ---- 4b. Nomes que o provedor não aceita ----
    // Uploads, pastas e moves para um caminho recusado ficam de fora com um
    // aviso (um só por subárvore); o resto do ciclo segue normalmente. Um
    // move recusado leva junto tudo abaixo do destino, para a base continuar
    // no caminho antigo e o move ser tentado de novo no ciclo seguinte.
    let mut rejected: Vec<RelPath> = Vec::new();
    let mut moved_away: Vec<RelPath> = Vec::new();
    let mut reject = |path: &str, rejected: &mut Vec<RelPath>| -> bool {
        if rejected.iter().any(|r| path == r || is_under(path, r)) {
            return true;
        }
        match check(path) {
            Ok(()) => false,
            Err(why) => {
                warnings.push(format!("não enviado: {path} ({why})"));
                rejected.push(path.to_string());
                true
            }
        }
    };
    actions.retain(|a| match a {
        Action::MoveRemote { to, .. } if reject(to, &mut rejected) => {
            moved_away.push(to.clone());
            false
        }
        _ => true,
    });
    per_path.retain(|a| {
        let path = a.path();
        if moved_away.iter().any(|m| path == m || is_under(path, m)) {
            return false;
        }
        !(matches!(a, Action::Upload { .. } | Action::CreateRemoteDir { .. }) && reject(path, &mut rejected))
    });

    // ---- 5. Ordenação ----
    per_path.sort_by(|a, b| {
        a.phase().cmp(&b.phase()).then_with(|| match a.phase() {
            // Exclusões: mais profundas primeiro.
            3 => depth(b.path()).cmp(&depth(a.path())).then(a.path().cmp(b.path())),
            _ => depth(a.path()).cmp(&depth(b.path())).then(a.path().cmp(b.path())),
        })
    });
    actions.extend(per_path);
    Plan { actions, local: local.into_iter().collect(), remote: remote.into_iter().collect(), warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_file(path: &str, id: &str, hash: &str, inode: u64) -> BaseEntry {
        BaseEntry {
            path: path.into(),
            is_dir: false,
            remote_id: id.into(),
            remote_rev: format!("rev-{hash}"),
            hash: Some(hash.into()),
            size: 10,
            local_mtime_ns: 100,
            inode,
        }
    }
    fn base_dir(path: &str, id: &str, inode: u64) -> BaseEntry {
        BaseEntry {
            path: path.into(),
            is_dir: true,
            remote_id: id.into(),
            remote_rev: "d".into(),
            hash: None,
            size: 0,
            local_mtime_ns: 1,
            inode,
        }
    }
    /// Arquivo local não modificado em relação a `base_file`.
    fn local_same(path: &str, inode: u64) -> LocalEntry {
        LocalEntry { path: path.into(), is_dir: false, size: 10, mtime_ns: 100, inode, hash: None }
    }
    fn local_mod(path: &str, inode: u64, hash: &str) -> LocalEntry {
        LocalEntry {
            path: path.into(),
            is_dir: false,
            size: 11,
            mtime_ns: 200,
            inode,
            hash: Some(hash.into()),
        }
    }
    fn local_dir(path: &str, inode: u64) -> LocalEntry {
        LocalEntry { path: path.into(), is_dir: true, size: 0, mtime_ns: 5, inode, hash: None }
    }
    fn remote_file(path: &str, id: &str, hash: &str) -> RemoteEntry {
        let (parent, name) = (parent_of(path).unwrap_or(""), name_of(path));
        RemoteEntry {
            path: path.into(),
            item: RemoteItem {
                id: id.into(),
                parent_id: Some(format!("id:{parent}")),
                name: name.into(),
                is_dir: false,
                size: 10,
                mtime_ms: 0,
                rev: format!("rev-{hash}"),
                hash: Some(hash.into()),
                native: false,
            },
        }
    }
    fn remote_dir(path: &str, id: &str) -> RemoteEntry {
        let mut r = remote_file(path, id, "x");
        r.item.is_dir = true;
        r.item.hash = None;
        r.item.rev = "d".into();
        r
    }

    fn snap(base: Vec<BaseEntry>, local: Vec<LocalEntry>, remote: Vec<RemoteEntry>) -> Snapshot {
        Snapshot {
            base: base.into_iter().map(|b| (b.path.clone(), b)).collect(),
            local: local.into_iter().map(|l| (l.path.clone(), l)).collect(),
            remote: remote.into_iter().map(|r| (r.path.clone(), r)).collect(),
        }
    }

    fn acts(s: Snapshot) -> Vec<Action> {
        plan(&s).actions
    }

    fn native(mut r: RemoteEntry) -> RemoteEntry {
        r.item.native = true;
        r
    }

    /// Regras simples de teste: recusa `:`, `CON` e caminhos acima de 20 caracteres.
    fn strict(path: &str) -> Result<(), String> {
        if path.chars().count() > 20 {
            return Err("caminho longo".into());
        }
        for seg in path.split('/') {
            if seg.contains(':') || seg == "CON" {
                return Err(format!("nome inválido: {seg}"));
            }
        }
        Ok(())
    }

    #[test]
    fn nome_recusado_vira_aviso_e_o_resto_segue() {
        let s = snap(
            vec![],
            vec![
                local_mod("relatório: final.txt", 1, "a"),
                local_mod("CON", 2, "b"),
                local_dir("pasta:x", 3),
                local_mod("pasta:x/a.txt", 4, "c"),
                local_mod("pasta:x/b.txt", 5, "c"),
                local_mod("nome-de-arquivo-comprido.txt", 6, "e"),
                local_mod("ok.txt", 7, "d"),
            ],
            vec![],
        );
        let p = plan_checked(&s, strict);
        assert_eq!(p.actions, vec![Action::Upload { path: "ok.txt".into() }]);
        assert_eq!(p.warnings.len(), 4, "{:?}", p.warnings);
        for name in ["relatório: final.txt", "CON", "nome-de-arquivo-comprido.txt"] {
            assert!(p.warnings.iter().any(|w| w.contains(name)), "{name}");
        }
        // Um aviso só para a pasta recusada, não um por arquivo dentro dela.
        assert!(p.warnings.iter().any(|w| w.starts_with("não enviado: pasta:x (")));
        // Sem validação, tudo sobe.
        assert_eq!(plan(&s).actions.len(), 7);
    }

    #[test]
    fn rename_local_para_nome_recusado_nao_mexe_no_remoto() {
        let mut x = local_same("E:/x", 11);
        x.hash = Some("h".into());
        let s = snap(
            vec![base_dir("D", "DD", 10), base_file("D/x", "X", "h", 11)],
            vec![local_dir("E:", 10), x],
            vec![remote_dir("D", "DD"), remote_file("D/x", "X", "h")],
        );
        let p = plan_checked(&s, strict);
        assert!(p.actions.is_empty(), "{:?}", p.actions);
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn atalho_editado_localmente_e_restaurado() {
        let s = snap(
            vec![base_file("R.gdoc", "D", "h", 1)],
            vec![local_mod("R.gdoc", 1, "editado")],
            vec![native(remote_file("R.gdoc", "D", "h"))],
        );
        assert_eq!(acts(s), vec![Action::Download { path: "R.gdoc".into() }]);
    }

    #[test]
    fn atalho_com_edicao_dupla_nao_vira_conflito() {
        let mut r = native(remote_file("R.gdoc", "D", "h"));
        r.item.rev = "rev-novo".into();
        r.item.hash = Some("h2".into());
        let s = snap(vec![base_file("R.gdoc", "D", "h", 1)], vec![local_mod("R.gdoc", 1, "editado")], vec![r]);
        assert_eq!(acts(s), vec![Action::Download { path: "R.gdoc".into() }]);
    }

    #[test]
    fn atalho_renomeado_localmente_renomeia_o_documento() {
        let s = snap(
            vec![base_file("R.gdoc", "D", "h", 7)],
            vec![local_same("Final.gdoc", 7)],
            vec![native(remote_file("R.gdoc", "D", "h"))],
        );
        assert_eq!(acts(s), vec![Action::MoveRemote { from: "R.gdoc".into(), to: "Final.gdoc".into() }]);
    }

    #[test]
    fn atalho_apagado_localmente_vai_para_a_lixeira() {
        let s = snap(vec![base_file("R.gdoc", "D", "h", 1)], vec![], vec![native(remote_file("R.gdoc", "D", "h"))]);
        assert_eq!(acts(s), vec![Action::DeleteRemote { path: "R.gdoc".into() }]);
    }

    #[test]
    fn atalho_sem_documento_remoto_e_ignorado() {
        let p = plan(&snap(vec![], vec![local_mod("Copiado.gdoc", 3, "x")], vec![]));
        assert!(p.actions.is_empty());
        assert_eq!(p.warnings.len(), 1);
        // Documento apagado no Drive e atalho editado: o atalho vai junto.
        let s = snap(vec![base_file("R.gdoc", "D", "h", 1)], vec![local_mod("R.gdoc", 1, "editado")], vec![]);
        assert_eq!(acts(s), vec![Action::DeleteLocal { path: "R.gdoc".into() }]);
    }

    #[test]
    fn in_sync_is_noop() {
        let s = snap(
            vec![base_file("a.txt", "A", "h1", 1)],
            vec![local_same("a.txt", 1)],
            vec![remote_file("a.txt", "A", "h1")],
        );
        assert!(acts(s).is_empty());
    }

    #[test]
    fn new_files_both_directions() {
        let mut l = local_mod("up.txt", 1, "h");
        l.hash = Some("h".into());
        let s = snap(vec![], vec![l], vec![remote_file("down.txt", "D", "h2")]);
        assert_eq!(
            acts(s),
            vec![Action::Download { path: "down.txt".into() }, Action::Upload { path: "up.txt".into() }]
        );
    }

    #[test]
    fn local_edit_uploads_remote_edit_downloads() {
        let s = snap(
            vec![base_file("a", "A", "h1", 1), base_file("b", "B", "h1", 2)],
            vec![local_mod("a", 1, "h2"), local_same("b", 2)],
            vec![remote_file("a", "A", "h1"), remote_file("b", "B", "h9")],
        );
        assert_eq!(
            acts(s),
            vec![Action::Upload { path: "a".into() }, Action::Download { path: "b".into() }]
        );
    }

    #[test]
    fn touch_without_content_change_only_records_base() {
        let mut l = local_mod("a", 1, "h1");
        l.size = 10;
        let s = snap(vec![base_file("a", "A", "h1", 1)], vec![l], vec![remote_file("a", "A", "h1")]);
        assert_eq!(acts(s), vec![Action::RecordBase { path: "a".into() }]);
    }

    #[test]
    fn both_edited_differently_is_conflict_same_is_record() {
        let s = snap(
            vec![base_file("a", "A", "h1", 1), base_file("b", "B", "h1", 2)],
            vec![local_mod("a", 1, "h2"), local_mod("b", 2, "h5")],
            vec![remote_file("a", "A", "h3"), remote_file("b", "B", "h5")],
        );
        assert_eq!(
            acts(s),
            vec![Action::Conflict { path: "a".into() }, Action::RecordBase { path: "b".into() }]
        );
    }

    #[test]
    fn new_on_both_sides_with_different_content_is_conflict() {
        let s = snap(vec![], vec![local_mod("a", 1, "x")], vec![remote_file("a", "A", "y")]);
        assert_eq!(acts(s), vec![Action::Conflict { path: "a".into() }]);
    }

    #[test]
    fn deletes_propagate() {
        let s = snap(
            vec![base_file("a", "A", "h", 1), base_file("b", "B", "h", 2)],
            vec![local_same("b", 2)],
            vec![remote_file("a", "A", "h")],
        );
        assert_eq!(
            acts(s),
            vec![Action::DeleteRemote { path: "a".into() }, Action::DeleteLocal { path: "b".into() }]
        );
    }

    #[test]
    fn edit_wins_over_delete() {
        let s = snap(
            vec![base_file("a", "A", "h", 1), base_file("b", "B", "h", 2)],
            vec![local_mod("b", 2, "h2")],
            vec![remote_file("a", "A", "h9")],
        );
        assert_eq!(
            acts(s),
            vec![Action::Download { path: "a".into() }, Action::Upload { path: "b".into() }]
        );
    }

    #[test]
    fn remote_rename_moves_local_without_transfer() {
        let s = snap(
            vec![base_file("old.txt", "A", "h", 1)],
            vec![local_same("old.txt", 1)],
            vec![remote_file("new.txt", "A", "h")],
        );
        assert_eq!(
            acts(s),
            vec![Action::MoveLocal { from: "old.txt".into(), to: "new.txt".into() }]
        );
    }

    #[test]
    fn remote_dir_rename_moves_subtree_once() {
        let s = snap(
            vec![base_dir("D", "DD", 10), base_file("D/x", "X", "h", 11)],
            vec![local_dir("D", 10), local_same("D/x", 11)],
            vec![remote_dir("E", "DD"), remote_file("E/x", "X", "h")],
        );
        assert_eq!(acts(s), vec![Action::MoveLocal { from: "D".into(), to: "E".into() }]);
    }

    #[test]
    fn local_rename_moves_remote_without_transfer() {
        let mut moved = local_same("new.txt", 1);
        moved.hash = Some("h".into());
        let s = snap(
            vec![base_file("old.txt", "A", "h", 1)],
            vec![moved],
            vec![remote_file("old.txt", "A", "h")],
        );
        assert_eq!(
            acts(s),
            vec![Action::MoveRemote { from: "old.txt".into(), to: "new.txt".into() }]
        );
    }

    #[test]
    fn local_dir_rename_moves_subtree_once() {
        let mut x = local_same("E/x", 11);
        x.hash = Some("h".into());
        let s = snap(
            vec![base_dir("D", "DD", 10), base_file("D/x", "X", "h", 11)],
            vec![local_dir("E", 10), x],
            vec![remote_dir("D", "DD"), remote_file("D/x", "X", "h")],
        );
        assert_eq!(acts(s), vec![Action::MoveRemote { from: "D".into(), to: "E".into() }]);
    }

    #[test]
    fn inode_reuse_is_not_a_move() {
        let s = snap(
            vec![base_file("old", "A", "h", 1)],
            vec![local_mod("other", 1, "zzz")],
            vec![remote_file("old", "A", "h")],
        );
        assert_eq!(
            acts(s),
            vec![Action::Upload { path: "other".into() }, Action::DeleteRemote { path: "old".into() }]
        );
    }

    #[test]
    fn deleted_dir_collapses_to_single_delete() {
        let s = snap(
            vec![base_dir("D", "DD", 10), base_file("D/x", "X", "h", 11), base_file("D/y", "Y", "h", 12)],
            vec![],
            vec![remote_dir("D", "DD"), remote_file("D/x", "X", "h"), remote_file("D/y", "Y", "h")],
        );
        assert_eq!(acts(s), vec![Action::DeleteRemote { path: "D".into() }]);
    }

    #[test]
    fn deleted_dir_with_remote_edit_inside_is_restored() {
        let s = snap(
            vec![base_dir("D", "DD", 10), base_file("D/x", "X", "h", 11), base_file("D/y", "Y", "h", 12)],
            vec![],
            vec![remote_dir("D", "DD"), remote_file("D/x", "X", "h2"), remote_file("D/y", "Y", "h")],
        );
        assert_eq!(
            acts(s),
            vec![
                Action::CreateLocalDir { path: "D".into() },
                Action::Download { path: "D/x".into() },
                Action::DeleteRemote { path: "D/y".into() },
            ]
        );
    }

    #[test]
    fn remote_deleted_dir_with_new_local_file_is_recreated() {
        let mut n = local_mod("D/new", 20, "n");
        n.hash = Some("n".into());
        let s = snap(
            vec![base_dir("D", "DD", 10), base_file("D/x", "X", "h", 11)],
            vec![local_dir("D", 10), local_same("D/x", 11), n],
            vec![],
        );
        assert_eq!(
            acts(s),
            vec![
                Action::CreateRemoteDir { path: "D".into() },
                Action::Upload { path: "D/new".into() },
                Action::DeleteLocal { path: "D/x".into() },
            ]
        );
    }

    #[test]
    fn dirs_created_before_contents() {
        let mut f = local_mod("A/B/f", 3, "h");
        f.hash = Some("h".into());
        let s = snap(vec![], vec![f, local_dir("A/B", 2), local_dir("A", 1)], vec![]);
        assert_eq!(
            acts(s),
            vec![
                Action::CreateRemoteDir { path: "A".into() },
                Action::CreateRemoteDir { path: "A/B".into() },
                Action::Upload { path: "A/B/f".into() },
            ]
        );
    }
}
