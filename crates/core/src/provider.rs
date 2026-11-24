use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::model::{HashKind, RemoteItem};

/// Lote de mudanças vindo da API incremental do provedor
/// (Drive `changes.list` / Graph `delta`).
#[derive(Debug, Default)]
pub struct ChangeBatch {
    /// Itens criados ou alterados (estado atual completo do item).
    pub upserts: Vec<RemoteItem>,
    /// Ids removidos (apagados, enviados à lixeira ou sem acesso).
    pub removed: Vec<String>,
    /// Cursor para a próxima consulta.
    pub cursor: String,
    /// O provedor invalidou o cursor: é preciso relistar tudo.
    pub reset: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Quota {
    pub used: u64,
    pub total: Option<u64>,
}

/// Abstração comum a Google Drive e OneDrive.
///
/// Os ids são opacos. O motor mantém a árvore remota (id → pai/nome) e só
/// pede ao provedor operações atômicas.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Formato do hash de conteúdo que o provedor expõe.
    fn hash_kind(&self) -> HashKind;

    /// Resolve um caminho (`""` = raiz do drive) para o item de pasta.
    async fn resolve_folder(&self, path: &str) -> Result<RemoteItem>;

    /// Cursor que representa "agora". Deve ser obtido **antes** de
    /// `list_tree` para nenhuma mudança se perder.
    async fn start_cursor(&self, root_id: &str) -> Result<String>;

    /// Lista recursivamente todo o conteúdo abaixo de `root_id`.
    async fn list_tree(&self, root_id: &str) -> Result<Vec<RemoteItem>>;

    /// Mudanças desde `cursor`. Pode retornar itens fora da raiz; o motor filtra.
    async fn changes(&self, root_id: &str, cursor: &str) -> Result<ChangeBatch>;

    /// Baixa o conteúdo do arquivo para `dest` (caminho temporário).
    async fn download(&self, item: &RemoteItem, dest: &Path) -> Result<()>;

    /// Envia `src`. Com `existing_id`, substitui o conteúdo (nova revisão).
    async fn upload(
        &self,
        parent_id: &str,
        name: &str,
        src: &Path,
        mtime_ms: i64,
        existing_id: Option<&str>,
    ) -> Result<RemoteItem>;

    async fn create_folder(&self, parent_id: &str, name: &str) -> Result<RemoteItem>;

    /// Renomeia e/ou move.
    async fn move_item(&self, id: &str, new_parent_id: &str, new_name: &str) -> Result<RemoteItem>;

    /// Envia para a lixeira do provedor (recuperável).
    async fn trash(&self, id: &str) -> Result<()>;

    async fn quota(&self) -> Result<Quota>;

    /// Confere se o provedor aceita o caminho (relativo à raiz do drive).
    /// `Err` traz o motivo; o planejador não envia o item e mostra um aviso.
    fn validate_name(&self, _path: &str) -> std::result::Result<(), String> {
        Ok(())
    }
}
