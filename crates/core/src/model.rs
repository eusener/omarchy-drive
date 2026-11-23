use serde::{Deserialize, Serialize};

/// Caminho relativo à raiz do par de sincronização, sempre com `/` e sem
/// barra inicial (ex.: `Documentos/nota.txt`).
pub type RelPath = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashKind {
    /// Google Drive (`md5Checksum`), hex minúsculo.
    Md5,
    /// OneDrive (`quickXorHash`), base64.
    QuickXor,
    /// Google Drive (`sha256Checksum`), hex minúsculo.
    Sha256,
}

/// Item remoto como mantido na árvore remota (por id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteItem {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modificação em milissegundos desde a época Unix.
    pub mtime_ms: i64,
    /// Revisão opaca do provedor (etag/version). Usada quando não há hash.
    pub rev: String,
    /// Hash de conteúdo no formato `Provider::hash_kind`.
    pub hash: Option<String>,
    /// Documento nativo do provedor (Google Docs etc.) sem conteúdo binário.
    pub native: bool,
}

/// Item remoto resolvido para um caminho dentro da raiz do par.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub path: RelPath,
    pub item: RemoteItem,
}

/// Arquivo/pasta encontrado na varredura local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEntry {
    pub path: RelPath,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ns: i64,
    pub inode: u64,
    /// Calculado só quando tamanho/mtime diferem da base (ou sem base).
    pub hash: Option<String>,
}

/// Último estado sincronizado conhecido de um caminho (o "ancestral comum").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseEntry {
    pub path: RelPath,
    pub is_dir: bool,
    pub remote_id: String,
    pub remote_rev: String,
    pub hash: Option<String>,
    pub size: u64,
    pub local_mtime_ns: i64,
    pub inode: u64,
}

pub fn parent_of(path: &str) -> Option<&str> {
    path.rfind('/').map(|i| &path[..i])
}

pub fn name_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[i + 1..]).unwrap_or(path)
}

pub fn join(parent: &str, name: &str) -> RelPath {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// `child` está estritamente dentro de `dir`?
pub fn is_under(child: &str, dir: &str) -> bool {
    child.len() > dir.len() && child.starts_with(dir) && child.as_bytes()[dir.len()] == b'/'
}

pub fn depth(path: &str) -> usize {
    path.matches('/').count()
}
