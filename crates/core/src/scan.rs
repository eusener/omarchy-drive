//! Varredura da pasta local.

use std::collections::HashMap;
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
            out.errors.push(format!("nome não UTF-8 ignorado: {:?}", ent.path()));
            continue;
        };
        let path = join(rel, &name);
        if ignore.is_ignored(&path) {
            continue;
        }
        // symlink_metadata: links simbólicos não são seguidos.
        let Ok(md) = ent.path().symlink_metadata() else {
            out.skipped.push(path);
            continue;
        };
        let ft = md.file_type();
        if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
            continue;
        }
        let mtime_ns = md.mtime() * 1_000_000_000 + md.mtime_nsec();
        if ft.is_file()
            && md.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|d| d < SETTLE)
        {
            out.skipped.push(path);
            continue;
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
                match hash_file(kind, &ent.path()) {
                    Ok(h) => e.hash = Some(h),
                    Err(err) => {
                        out.errors.push(format!("{path}: {err}"));
                        out.skipped.push(path);
                        continue;
                    }
                }
            }
        }
        out.entries.insert(path.clone(), e);
        if ft.is_dir() {
            walk(&ent.path(), &path, ignore, base, kind, now, out);
        }
    }
}
