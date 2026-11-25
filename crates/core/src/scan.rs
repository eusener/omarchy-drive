//! Varredura da pasta local.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::hash::hash_file;
use crate::ignore::Ignore;
use crate::model::{BaseEntry, HashKind, LocalEntry, RelPath, join};

/// Arquivos modificados há menos que isso ainda podem estar sendo escritos;
/// ficam para o próximo ciclo.
pub const SETTLE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct ScanResult {
    pub entries: HashMap<RelPath, LocalEntry>,
    /// Caminhos (e subárvores) cujo estado é desconhecido neste ciclo:
    /// arquivos sendo escritos, pastas ilegíveis, erros de leitura. O motor
    /// os exclui do plano para que **nunca** sejam tratados como apagados.
    pub skipped: Vec<RelPath>,
    /// Mensagens de erro para o log/status.
    pub errors: Vec<String>,
}

/// Varre `root` (bloqueante). O hash só é calculado quando tamanho/mtime
/// diferem da base, para não reler o disco inteiro a cada ciclo.
pub fn scan(
    root: &Path,
    ignore: &Ignore,
    base: &HashMap<RelPath, BaseEntry>,
    kind: HashKind,
) -> ScanResult {
    let mut out = ScanResult::default();
    let now = SystemTime::now();
    walk(root, "", ignore, base, kind, now, &mut out);
    out
}

fn walk(
    abs: &Path,
    rel: &str,
    ignore: &Ignore,
    base: &HashMap<RelPath, BaseEntry>,
    kind: HashKind,
    now: SystemTime,
    out: &mut ScanResult,
) {
    let rd = match std::fs::read_dir(abs) {
        Ok(rd) => rd,
        Err(e) => {
            out.errors.push(format!("{}: {e}", abs.display()));
            out.skipped.push(rel.to_string());
            return;
        }
    };
    for ent in rd.flatten() {
        let Some(name) = ent.file_name().to_str().map(str::to_owned) else {
            out.errors
                .push(format!("nome não UTF-8 ignorado: {:?}", ent.path()));
            continue;
        };
        let path = join(rel, &name);
        if ignore.is_ignored(&path) {
            continue;
        }
        visit(&ent.path(), path, ignore, base, kind, now, out);
    }
}

/// Registra um caminho (e, se for pasta, a subárvore). Links simbólicos e
/// arquivos especiais são ignorados; arquivos ainda sendo escritos e erros de
/// leitura vão para `skipped`.
#[allow(clippy::too_many_arguments)]
fn visit(
    abs: &Path,
    path: RelPath,
    ignore: &Ignore,
    base: &HashMap<RelPath, BaseEntry>,
    kind: HashKind,
    now: SystemTime,
    out: &mut ScanResult,
) {
    // symlink_metadata: links simbólicos não são seguidos.
    let Ok(md) = abs.symlink_metadata() else {
        out.skipped.push(path);
        return;
    };
    let ft = md.file_type();
    if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
        return;
    }
    let mtime_ns = md.mtime() * 1_000_000_000 + md.mtime_nsec();
    if ft.is_file()
        && md
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|d| d < SETTLE)
    {
        out.skipped.push(path);
        return;
    }
    let mut e = LocalEntry {
        path: path.clone(),
        is_dir: ft.is_dir(),
        size: if ft.is_dir() { 0 } else { md.len() },
        mtime_ns,
        inode: md.ino(),
        hash: None,
    };
    if ft.is_file() {
        let needs = match base.get(&path) {
            Some(b) => b.size != e.size || b.local_mtime_ns != e.mtime_ns,
            None => true,
        };
        if needs {
            match hash_file(kind, abs) {
                Ok(h) => e.hash = Some(h),
                Err(err) => {
                    out.errors.push(format!("{path}: {err}"));
                    out.skipped.push(path);
                    return;
                }
            }
        }
    }
    out.entries.insert(path.clone(), e);
    if ft.is_dir() {
        walk(abs, &path, ignore, base, kind, now, out);
    }
}

/// Varredura parcial: parte do último resultado conhecido (`prev`) e relê do
/// disco só os caminhos em `dirty` (com as subárvores das pastas). Um caminho
/// só sai do resultado se o disco confirmar que ele não existe: o ancestral
/// mais próximo que existe precisa ser legível, senão ele vai para `skipped`.
pub fn scan_paths(
    root: &Path,
    ignore: &Ignore,
    base: &HashMap<RelPath, BaseEntry>,
    kind: HashKind,
    prev: &HashMap<RelPath, LocalEntry>,
    dirty: &HashSet<RelPath>,
) -> ScanResult {
    let mut out = ScanResult {
        entries: prev.clone(),
        ..Default::default()
    };
    let now = SystemTime::now();

    // Um caminho sob outro caminho sujo já é coberto pela subárvore dele.
    let mut paths: Vec<&RelPath> = dirty.iter().filter(|p| !p.is_empty()).collect();
    paths.sort();
    let mut roots: Vec<&RelPath> = Vec::new();
    for p in paths {
        if !roots.last().is_some_and(|r| crate::model::is_under(p, r)) {
            roots.push(p);
        }
    }

    for p in roots {
        if ignore.is_ignored(p) {
            continue;
        }
        forget_subtree(&mut out.entries, p);
        let skipped_before = out.skipped.len();
        let abs = root.join(p);
        match abs.symlink_metadata() {
            Ok(_) => visit(&abs, p.clone(), ignore, base, kind, now, &mut out),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                confirm_missing(root, p, &mut out);
            }
            Err(e) => {
                out.errors.push(format!("{p}: {e}"));
                out.skipped.push(p.clone());
            }
        }
        // Estado desconhecido: mantém o que se sabia antes (o motor tira o
        // caminho do plano, e a próxima varredura o relê).
        for sk in &out.skipped[skipped_before..] {
            for (k, v) in prev {
                if (k == sk || crate::model::is_under(k, sk)) && !out.entries.contains_key(k) {
                    out.entries.insert(k.clone(), v.clone());
                }
            }
        }
        // A pasta pai mudou de mtime; atualiza a entrada dela (sem descer).
        if let Some(parent) = crate::model::parent_of(p)
            && let Some(old) = out.entries.get_mut(parent)
            && let Ok(md) = root.join(parent).symlink_metadata()
            && md.is_dir()
        {
            old.mtime_ns = md.mtime() * 1_000_000_000 + md.mtime_nsec();
        }
    }
    out
}

fn forget_subtree(entries: &mut HashMap<RelPath, LocalEntry>, p: &str) {
    entries.retain(|k, _| k != p && !crate::model::is_under(k, p));
}

/// `p` não existe. Sobe até o ancestral que existe: os ancestrais ausentes
/// também saem do resultado; se o ancestral existente não puder ser lido, a
/// ausência não está confirmada e a subárvore fica fora do ciclo.
fn confirm_missing(root: &Path, p: &str, out: &mut ScanResult) {
    let mut cur = p;
    while let Some(parent) = crate::model::parent_of(cur) {
        match root.join(parent).symlink_metadata() {
            Ok(md) if md.is_dir() => {
                if std::fs::read_dir(root.join(parent)).is_err() {
                    out.skipped.push(parent.to_string());
                }
                return;
            }
            Ok(_) | Err(_) => {
                forget_subtree(&mut out.entries, parent);
                cur = parent;
            }
        }
    }
    if std::fs::read_dir(root).is_err() {
        out.skipped.push(String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TICK: AtomicU64 = AtomicU64::new(0);

    /// Grava com mtime no passado (fora da janela de SETTLE), sempre distinto.
    fn put(root: &Path, rel: &str, data: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, data).unwrap();
        let n = TICK.fetch_add(1, Ordering::SeqCst);
        let t = SystemTime::now() - Duration::from_secs(100_000) + Duration::from_secs(n);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn ign() -> Ignore {
        Ignore::new(&[], &[]).unwrap()
    }

    /// Base "sincronizada" a partir de uma varredura: o que não mudar depois
    /// não precisa de hash, como no motor.
    fn base_from(r: &ScanResult) -> HashMap<RelPath, BaseEntry> {
        r.entries
            .values()
            .map(|e| {
                (
                    e.path.clone(),
                    BaseEntry {
                        path: e.path.clone(),
                        is_dir: e.is_dir,
                        remote_id: format!("id:{}", e.path),
                        remote_rev: "1".into(),
                        hash: e.hash.clone(),
                        size: e.size,
                        local_mtime_ns: e.mtime_ns,
                        inode: e.inode,
                    },
                )
            })
            .collect()
    }

    /// Estado logo após uma sincronização: base e varredura feita com ela.
    fn synced(root: &Path) -> (ScanResult, HashMap<RelPath, BaseEntry>) {
        let base = base_from(&scan(root, &ign(), &HashMap::new(), HashKind::Md5));
        (scan(root, &ign(), &base, HashKind::Md5), base)
    }

    fn dirty(paths: &[&str]) -> HashSet<RelPath> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn assert_same(
        root: &Path,
        base: &HashMap<RelPath, BaseEntry>,
        prev: &ScanResult,
        d: &HashSet<RelPath>,
    ) {
        let full = scan(root, &ign(), base, HashKind::Md5);
        let part = scan_paths(root, &ign(), base, HashKind::Md5, &prev.entries, d);
        let mut a: Vec<_> = full.entries.values().map(|e| format!("{e:?}")).collect();
        let mut b: Vec<_> = part.entries.values().map(|e| format!("{e:?}")).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "dirty={d:?}");
        assert!(part.skipped.is_empty(), "{:?}", part.skipped);
    }

    #[test]
    fn criacao_edicao_e_exclusao() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "a.txt", b"A");
        put(root, "P/b.txt", b"B");
        let (prev, base) = synced(root);

        put(root, "P/novo.txt", b"N");
        put(root, "a.txt", b"AA");
        std::fs::remove_file(root.join("P/b.txt")).unwrap();
        let d = dirty(&["P/novo.txt", "a.txt", "P/b.txt"]);
        assert_same(root, &base, &prev, &d);
        let part = scan_paths(root, &ign(), &base, HashKind::Md5, &prev.entries, &d);
        assert!(part.entries.contains_key("P/novo.txt"));
        assert!(!part.entries.contains_key("P/b.txt"));
        assert!(part.entries["a.txt"].hash.is_some());
    }

    #[test]
    fn editor_que_salva_por_rename_nao_vira_exclusao() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "doc.md", b"v1");
        let (prev, base) = synced(root);
        // Evento de remoção chegou, mas o arquivo já foi recriado.
        put(root, ".doc.md.swp", b"v2");
        std::fs::rename(root.join(".doc.md.swp"), root.join("doc.md")).unwrap();
        let part = scan_paths(
            root,
            &ign(),
            &base,
            HashKind::Md5,
            &prev.entries,
            &dirty(&["doc.md"]),
        );
        assert!(part.entries.contains_key("doc.md"));
        assert_same(root, &base, &prev, &dirty(&["doc.md", ".doc.md.swp"]));
    }

    #[test]
    fn pasta_movida_para_dentro_e_varrida_inteira() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("par");
        let fora = dir.path().join("fora");
        std::fs::create_dir_all(&root).unwrap();
        put(&root, "x.txt", b"x");
        let (prev, base) = synced(&root);
        put(&fora, "Proj/src/main.rs", b"fn main(){}");
        put(&fora, "Proj/README", b"r");
        std::fs::rename(fora.join("Proj"), root.join("Proj")).unwrap();
        let part = scan_paths(
            &root,
            &ign(),
            &base,
            HashKind::Md5,
            &prev.entries,
            &dirty(&["Proj"]),
        );
        assert!(part.entries.contains_key("Proj/src/main.rs"));
        assert!(part.entries.contains_key("Proj/README"));
        assert_same(&root, &base, &prev, &dirty(&["Proj"]));
    }

    #[test]
    fn pai_apagado_remove_a_subarvore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "A/B/c.txt", b"c");
        put(root, "A/d.txt", b"d");
        put(root, "keep.txt", b"k");
        let (prev, base) = synced(root);
        std::fs::remove_dir_all(root.join("A")).unwrap();
        // Só o evento do arquivo mais fundo chegou.
        let part = scan_paths(
            root,
            &ign(),
            &base,
            HashKind::Md5,
            &prev.entries,
            &dirty(&["A/B/c.txt"]),
        );
        assert!(
            part.entries.keys().all(|k| !k.starts_with('A')),
            "{:?}",
            part.entries.keys()
        );
        assert!(part.entries.contains_key("keep.txt"));
    }

    #[test]
    fn pai_ilegivel_nao_confirma_exclusao() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "S/a.txt", b"a");
        let (prev, base) = synced(root);
        std::fs::remove_file(root.join("S/a.txt")).unwrap();
        std::fs::set_permissions(root.join("S"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let part = scan_paths(
            root,
            &ign(),
            &base,
            HashKind::Md5,
            &prev.entries,
            &dirty(&["S/a.txt"]),
        );
        std::fs::set_permissions(root.join("S"), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root lê pastas sem permissão; nesse caso o teste não se aplica.
        if unsafe { libc_geteuid() } != 0 {
            let covered = part.skipped.iter().any(|s| s == "S" || s == "S/a.txt");
            assert!(covered, "{:?}", part.skipped);
            assert!(
                part.entries.contains_key("S/a.txt"),
                "ausência não confirmada não pode virar exclusão"
            );
        }
    }

    unsafe extern "C" {
        #[link_name = "geteuid"]
        fn libc_geteuid() -> u32;
    }

    /// Gerador pseudoaleatório determinístico (sem dependências).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    /// Sequências aleatórias de operações: a varredura parcial (com os
    /// caminhos que o inotify reportaria) tem de chegar ao mesmo resultado
    /// que a completa.
    #[test]
    fn equivalencia_com_varredura_completa() {
        for seed in 1..=40u64 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let mut rng = Lcg(seed);
            let names = ["a", "b", "c", "D", "E/f", "E/g", "H/I/j"];
            for n in &names[..4] {
                put(root, &format!("{n}.txt"), n.as_bytes());
            }
            let (mut prev, base) = synced(root);
            for _round in 0..5 {
                let mut d = HashSet::new();
                for _ in 0..1 + rng.next(4) {
                    let n = names[rng.next(names.len())];
                    let f = format!("{n}.txt");
                    match rng.next(6) {
                        0 | 1 => {
                            put(root, &f, format!("{seed}-{}", rng.next(1000)).as_bytes());
                            // Criar arquivo em pasta nova cria as pastas também.
                            let mut cur = f.as_str();
                            while let Some(p) = crate::model::parent_of(cur) {
                                d.insert(p.to_string());
                                cur = p;
                            }
                            d.insert(f);
                        }
                        2 => {
                            let _ = std::fs::remove_file(root.join(&f));
                            d.insert(f);
                        }
                        3 => {
                            let to = format!("{}.txt", names[rng.next(names.len())]);
                            if root.join(&f).is_file() && root.join(&to).parent().unwrap().is_dir()
                            {
                                std::fs::rename(root.join(&f), root.join(&to)).unwrap();
                                d.insert(f);
                                d.insert(to);
                            }
                        }
                        4 => {
                            for top in ["E", "H"] {
                                if rng.next(2) == 0 && root.join(top).is_dir() {
                                    std::fs::remove_dir_all(root.join(top)).unwrap();
                                    d.insert(top.to_string());
                                }
                            }
                        }
                        _ => {
                            if root.join("E").is_dir() && !root.join("Z").exists() {
                                std::fs::rename(root.join("E"), root.join("Z")).unwrap();
                                d.insert("E".into());
                                d.insert("Z".into());
                            } else if root.join("Z").is_dir() && !root.join("E").exists() {
                                std::fs::rename(root.join("Z"), root.join("E")).unwrap();
                                d.insert("E".into());
                                d.insert("Z".into());
                            }
                        }
                    }
                }
                assert_same(root, &base, &prev, &d);
                prev = scan_paths(root, &ign(), &base, HashKind::Md5, &prev.entries, &d);
            }
        }
    }
}

/// Medição da varredura numa árvore grande. Rode com:
/// `cargo test -p omarchy-drive-core --release bench_ -- --ignored --nocapture`
#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn bench_varredura_50_mil_arquivos() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let old = SystemTime::now() - Duration::from_secs(3600);
        for d in 0..500 {
            let sub = root.join(format!("pasta{d:03}/sub"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..100 {
                let p = sub.join(format!("arquivo{f:03}.txt"));
                std::fs::write(&p, format!("{d}-{f}")).unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
        }
        let ign = Ignore::new(&[], &[]).unwrap();
        // Base sincronizada: a varredura completa não precisa recalcular hashes.
        let first = scan(root, &ign, &HashMap::new(), HashKind::Md5);
        let base: HashMap<RelPath, BaseEntry> = first
            .entries
            .values()
            .map(|e| {
                (
                    e.path.clone(),
                    BaseEntry {
                        path: e.path.clone(),
                        is_dir: e.is_dir,
                        remote_id: String::new(),
                        remote_rev: String::new(),
                        hash: e.hash.clone(),
                        size: e.size,
                        local_mtime_ns: e.mtime_ns,
                        inode: e.inode,
                    },
                )
            })
            .collect();
        let n = first.entries.len();

        let t = Instant::now();
        let full = scan(root, &ign, &base, HashKind::Md5);
        let full_t = t.elapsed();

        std::fs::write(root.join("pasta123/sub/arquivo045.txt"), "editado").unwrap();
        let mut dirty = HashSet::new();
        dirty.insert("pasta123/sub/arquivo045.txt".to_string());
        let t = Instant::now();
        let part = scan_paths(root, &ign, &base, HashKind::Md5, &full.entries, &dirty);
        let part_t = t.elapsed();

        let t = Instant::now();
        let idle = scan_paths(
            root,
            &ign,
            &base,
            HashKind::Md5,
            &full.entries,
            &HashSet::new(),
        );
        let idle_t = t.elapsed();

        println!("entradas: {n}");
        println!("varredura completa (base em dia): {full_t:?}");
        println!("parcial, 1 arquivo editado:       {part_t:?}");
        println!("parcial, sem eventos:             {idle_t:?}");
        assert_eq!(part.entries.len(), n);
        assert_eq!(idle.entries.len(), n);
    }
}
