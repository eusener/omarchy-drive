//! Provedor OneDrive (Microsoft Graph v1.0).
//!
//! Usa sempre o `delta` da raiz do drive (o `delta` por pasta não existe no
//! OneDrive for Business); o motor filtra por ancestralidade.

mod graph;

use std::io::SeekFrom;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use omarchy_drive_core::http::{self, HttpError};
use omarchy_drive_core::oauth::{OAuthEndpoints, TokenManager};
use omarchy_drive_core::{ChangeBatch, HashKind, Provider, Quota, RemoteItem};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::OnceCell;
use tracing::{debug, warn};

use graph::{
    DeltaAccumulator, DriveItem, Page, PageNext, SELECT, encode_path, encode_segment,
    format_time_ms, is_resync_body, live_items, next_expected_start, to_remote,
};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const BASE: &str = "https://graph.microsoft.com/v1.0/me/drive";

/// Até este tamanho usa upload simples (`PUT .../content`).
const SIMPLE_UPLOAD_MAX: u64 = 4 * 1024 * 1024;
/// Tamanho dos pedaços da sessão de upload (múltiplo de 320 KiB).
const CHUNK: u64 = 10 * 1024 * 1024;
const _: () = assert!(CHUNK.is_multiple_of(320 * 1024));
/// Pastas listadas em paralelo na busca em largura.
const LIST_CONCURRENCY: usize = 4;

/// Endpoints OAuth da plataforma de identidade da Microsoft (cliente público).
pub fn endpoints(client_id: String) -> OAuthEndpoints {
    OAuthEndpoints {
        auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
        client_id,
        client_secret: None,
        scopes: ["Files.ReadWrite.All", "offline_access", "User.Read"]
            .map(String::from)
            .to_vec(),
        redirect_host: "localhost".into(),
        extra_auth_params: Vec::new(),
    }
}

pub struct OneDrive {
    tokens: TokenManager,
    /// Cliente para a API (com bearer via `http::send`). Redirecionamentos
    /// para outro host (download pré-autenticado) têm o `Authorization`
    /// removido pelo próprio reqwest.
    http: reqwest::Client,
    /// Cliente para as URLs pré-autenticadas da sessão de upload, que
    /// **não** podem receber o token.
    raw: reqwest::Client,
    root_id: OnceCell<String>,
}

impl OneDrive {
    pub fn new(tokens: TokenManager) -> Self {
        let build = || {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(300))
                .build()
                .expect("cliente HTTP")
        };
        Self { tokens, http: build(), raw: build(), root_id: OnceCell::new() }
    }

    /// E-mail da conta (`mail`, ou `userPrincipalName` se ausente).
    pub async fn account_email(&self) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Me {
            mail: Option<String>,
            user_principal_name: Option<String>,
        }
        let me: Me = self.get_json(&format!("{GRAPH}/me?$select=mail,userPrincipalName")).await?;
        me.mail
            .filter(|m| !m.is_empty())
            .or(me.user_principal_name)
            .context("a conta Microsoft não informou e-mail")
    }

    // ------------------------------------------------------------------
    // Auxiliares
    // ------------------------------------------------------------------

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let r = http::send(&self.tokens, || self.http.get(url)).await?;
        Ok(r.json().await?)
    }

    async fn get_item(&self, url: &str) -> Result<RemoteItem> {
        let it: DriveItem = self.get_json(url).await?;
        to_remote(&it)
    }

    async fn root_item(&self) -> Result<RemoteItem> {
        let root = self.get_item(&format!("{BASE}/root?$select={SELECT}")).await?;
        let _ = self.root_id.set(root.id.clone());
        Ok(root)
    }

    async fn drive_root_id(&self) -> Result<String> {
        if let Some(id) = self.root_id.get() {
            return Ok(id.clone());
        }
        Ok(self.root_item().await?.id)
    }

    async fn child_by_name(&self, parent_id: &str, name: &str) -> Result<RemoteItem> {
        self.get_item(&format!("{BASE}/items/{parent_id}:/{}:?$select={SELECT}", encode_segment(name)))
            .await
    }

    async fn list_children(&self, id: &str) -> Result<Vec<RemoteItem>> {
        let mut url = format!("{BASE}/items/{id}/children?$top=1000&$select={SELECT}");
        let mut out = Vec::new();
        loop {
            let page: Page = self.get_json(&url).await?;
            out.extend(live_items(&page)?);
            match page.next_link {
                Some(n) => url = n,
                None => return Ok(out),
            }
        }
    }

    /// Listagem completa do drive via `delta` (bem mais rápida que a busca
    /// em largura quando a raiz do par é a raiz do drive).
    async fn list_all_delta(&self) -> Result<Vec<RemoteItem>> {
        let mut url = format!("{BASE}/root/delta?$select={SELECT}");
        let mut out = Vec::new();
        loop {
            let page: Page = self.get_json(&url).await?;
            out.extend(live_items(&page)?);
            match page.next()? {
                PageNext::More(n) => url = n,
                PageNext::Done(_) => return Ok(out),
            }
        }
    }

    async fn patch_mtime(&self, id: &str, mtime_ms: i64) -> Result<RemoteItem> {
        let body = json!({ "fileSystemInfo": { "lastModifiedDateTime": format_time_ms(mtime_ms) } });
        let url = format!("{BASE}/items/{id}");
        let r = http::send(&self.tokens, || self.http.patch(&url).json(&body)).await?;
        let it: DriveItem = r.json().await?;
        to_remote(&it)
    }

    async fn upload_simple(
        &self,
        parent_id: &str,
        name: &str,
        src: &Path,
        mtime_ms: i64,
        existing_id: Option<&str>,
    ) -> Result<RemoteItem> {
        let data = bytes::Bytes::from(tokio::fs::read(src).await?);
        let url = match existing_id {
            Some(id) => format!("{BASE}/items/{id}/content"),
            None => format!(
                "{BASE}/items/{parent_id}:/{}:/content?@microsoft.graph.conflictBehavior=replace",
                encode_segment(name)
            ),
        };
        let r = http::send(&self.tokens, || {
            self.http
                .put(&url)
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(data.clone())
        })
        .await?;
        let it: DriveItem = r.json().await?;
        self.patch_mtime(&it.id, mtime_ms).await
    }

    async fn upload_session(
        &self,
        parent_id: &str,
        name: &str,
        src: &Path,
        total: u64,
        mtime_ms: i64,
        existing_id: Option<&str>,
    ) -> Result<RemoteItem> {
        let create = match existing_id {
            Some(id) => format!("{BASE}/items/{id}/createUploadSession"),
            None => format!("{BASE}/items/{parent_id}:/{}:/createUploadSession", encode_segment(name)),
        };
        let body = json!({
            "item": {
                "@microsoft.graph.conflictBehavior": "replace",
                "fileSystemInfo": { "lastModifiedDateTime": format_time_ms(mtime_ms) }
            }
        });
        let r = http::send(&self.tokens, || self.http.post(&create).json(&body)).await?;
        let session: Value = r.json().await?;
        let upload_url = session
            .get("uploadUrl")
            .and_then(Value::as_str)
            .context("createUploadSession sem uploadUrl")?
            .to_string();

        let res = self.send_chunks(&upload_url, src, total).await;
        let item = match res {
            Ok(it) => it,
            Err(e) => {
                // Melhor esforço: cancela a sessão para não deixar lixo.
                let _ = self.raw.delete(&upload_url).send().await;
                return Err(e);
            }
        };
        // Garante a data de modificação caso o servidor tenha ignorado o
        // `fileSystemInfo` da sessão.
        if item.mtime_ms / 1000 != mtime_ms / 1000 {
            return self.patch_mtime(&item.id, mtime_ms).await;
        }
        Ok(item)
    }

    /// Envia os pedaços para a URL da sessão (sem `Authorization`).
    async fn send_chunks(&self, upload_url: &str, src: &Path, total: u64) -> Result<RemoteItem> {
        let mut file = tokio::fs::File::open(src).await?;
        let mut offset = 0u64;
        let mut failures = 0u32;
        let mut delay = Duration::from_millis(500);
        loop {
            if offset >= total {
                bail!("sessão de upload sem resposta final após enviar {total} bytes");
            }
            let len = CHUNK.min(total - offset);
            file.seek(SeekFrom::Start(offset)).await?;
            let mut buf = vec![0u8; len as usize];
            file.read_exact(&mut buf).await.context("arquivo encolheu durante o envio")?;
            let res = self
                .raw
                .put(upload_url)
                .header(CONTENT_RANGE, format!("bytes {offset}-{}/{total}", offset + len - 1))
                .header(CONTENT_LENGTH, len)
                .body(buf)
                .send()
                .await;

            let mut retry_after = None;
            match res {
                Ok(r) if r.status() == StatusCode::ACCEPTED => {
                    let v: Value = r.json().await?;
                    offset = next_expected_start(&v).unwrap_or(offset + len);
                    failures = 0;
                    delay = Duration::from_millis(500);
                    continue;
                }
                Ok(r) if r.status() == StatusCode::OK || r.status() == StatusCode::CREATED => {
                    let it: DriveItem = r.json().await?;
                    return to_remote(&it);
                }
                Ok(r)
                    if r.status().is_server_error()
                        || r.status() == StatusCode::TOO_MANY_REQUESTS
                        || r.status() == StatusCode::RANGE_NOT_SATISFIABLE =>
                {
                    retry_after = r
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    debug!(status = %r.status(), "falha transitória no pedaço do upload");
                }
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    return Err(HttpError { status, body }.into());
                }
                Err(e) => {
                    debug!(error = %e, "erro de rede no pedaço do upload");
                }
            }

            failures += 1;
            if failures >= 8 {
                bail!("upload abortado: falhas repetidas na sessão de envio");
            }
            tokio::time::sleep(retry_after.unwrap_or(delay)).await;
            delay = (delay * 2).min(Duration::from_secs(64));
            // Pergunta à sessão de onde continuar.
            match self.session_offset(upload_url).await {
                Ok(Some(o)) => offset = o,
                Ok(None) => {}
                Err(e) => warn!(error = %e, "não foi possível consultar a sessão de upload"),
            }
        }
    }

    async fn session_offset(&self, upload_url: &str) -> Result<Option<u64>> {
        let r = self.raw.get(upload_url).send().await?;
        let status = r.status();
        if !status.is_success() {
            let body = r.text().await.unwrap_or_default();
            if status == StatusCode::NOT_FOUND {
                bail!("sessão de upload expirou: {body}");
            }
            return Err(HttpError { status, body }.into());
        }
        let v: Value = r.json().await?;
        Ok(next_expected_start(&v))
    }
}

fn is_resync(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<HttpError>() {
        Some(h) => h.status == StatusCode::GONE || is_resync_body(&h.body),
        None => false,
    }
}

#[async_trait]
impl Provider for OneDrive {
    fn name(&self) -> &'static str {
        "onedrive"
    }

    fn hash_kind(&self) -> HashKind {
        HashKind::QuickXor
    }

    async fn resolve_folder(&self, path: &str) -> Result<RemoteItem> {
        let enc = encode_path(path);
        if enc.is_empty() {
            return self.root_item().await;
        }
        let item = match self.get_item(&format!("{BASE}/root:/{enc}:?$select={SELECT}")).await {
            Ok(it) => it,
            Err(e) if http::status_of(&e) == Some(StatusCode::NOT_FOUND) => {
                // Cria os segmentos que faltam, um a um.
                let mut cur = self.root_item().await?;
                for seg in path.split('/').filter(|s| !s.is_empty()) {
                    cur = self.create_folder(&cur.id, seg).await?;
                }
                cur
            }
            Err(e) => return Err(e),
        };
        if !item.is_dir {
            bail!("o caminho remoto \"{path}\" não é uma pasta");
        }
        Ok(item)
    }

    async fn start_cursor(&self, _root_id: &str) -> Result<String> {
        let page: Page = self.get_json(&format!("{BASE}/root/delta?token=latest&$select={SELECT}")).await?;
        match page.next()? {
            PageNext::Done(c) => Ok(c),
            // Não deveria acontecer com token=latest; segue até o fim.
            PageNext::More(mut url) => loop {
                let p: Page = self.get_json(&url).await?;
                match p.next()? {
                    PageNext::Done(c) => return Ok(c),
                    PageNext::More(n) => url = n,
                }
            },
        }
    }

    async fn list_tree(&self, root_id: &str) -> Result<Vec<RemoteItem>> {
        if root_id == self.drive_root_id().await? {
            return self.list_all_delta().await;
        }
        let mut out = Vec::new();
        let mut frontier = vec![root_id.to_string()];
        while !frontier.is_empty() {
            let level: Vec<Vec<RemoteItem>> = futures::stream::iter(std::mem::take(&mut frontier))
                .map(|id| async move { self.list_children(&id).await })
                .buffer_unordered(LIST_CONCURRENCY)
                .try_collect()
                .await?;
            for items in level {
                frontier.extend(items.iter().filter(|i| i.is_dir).map(|i| i.id.clone()));
                out.extend(items);
            }
        }
        Ok(out)
    }

    async fn changes(&self, _root_id: &str, cursor: &str) -> Result<ChangeBatch> {
        let mut acc = DeltaAccumulator::default();
        let mut url = cursor.to_string();
        loop {
            let page: Page = match self.get_json(&url).await {
                Ok(p) => p,
                Err(e) if is_resync(&e) => {
                    debug!(error = %e, "cursor de delta invalidado; relistando");
                    return Ok(ChangeBatch { reset: true, ..Default::default() });
                }
                Err(e) => return Err(e),
            };
            acc.add_page(&page)?;
            match page.next()? {
                PageNext::More(n) => url = n,
                PageNext::Done(c) => return Ok(acc.finish(c)),
            }
        }
    }

    async fn download(&self, item: &RemoteItem, dest: &Path) -> Result<()> {
        if item.is_dir || item.native {
            bail!("\"{}\" não tem conteúdo para baixar", item.name);
        }
        let url = format!("{BASE}/items/{}/content", item.id);
        // O Graph responde 302 para uma URL pré-autenticada em outro host;
        // o reqwest segue e remove o Authorization (troca de host).
        let r = http::send(&self.tokens, || self.http.get(&url)).await?;
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = r.bytes_stream();
        // Sem checar `item.size`: no OneDrive for Business arquivos do Office
        // podem vir com tamanho diferente do informado (metadados do
        // SharePoint). O motor confere o hash depois.
        while let Some(chunk) = stream.next().await {
            file.write_all(&chunk?).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        Ok(())
    }

    async fn upload(
        &self,
        parent_id: &str,
        name: &str,
        src: &Path,
        mtime_ms: i64,
        existing_id: Option<&str>,
    ) -> Result<RemoteItem> {
        let size = tokio::fs::metadata(src).await?.len();
        if size <= SIMPLE_UPLOAD_MAX {
            self.upload_simple(parent_id, name, src, mtime_ms, existing_id).await
        } else {
            self.upload_session(parent_id, name, src, size, mtime_ms, existing_id).await
        }
    }

    async fn create_folder(&self, parent_id: &str, name: &str) -> Result<RemoteItem> {
        let url = format!("{BASE}/items/{parent_id}/children");
        let body = json!({
            "name": name,
            "folder": {},
            "@microsoft.graph.conflictBehavior": "fail"
        });
        let item = match http::send(&self.tokens, || self.http.post(&url).json(&body)).await {
            Ok(r) => to_remote(&r.json::<DriveItem>().await?)?,
            Err(e) if http::status_of(&e) == Some(StatusCode::CONFLICT) => {
                self.child_by_name(parent_id, name).await?
            }
            Err(e) => return Err(e),
        };
        if !item.is_dir {
            bail!("já existe um arquivo chamado \"{name}\" onde deveria haver uma pasta");
        }
        Ok(item)
    }

    async fn move_item(&self, id: &str, new_parent_id: &str, new_name: &str) -> Result<RemoteItem> {
        let url = format!("{BASE}/items/{id}");
        let body = json!({ "name": new_name, "parentReference": { "id": new_parent_id } });
        let r = http::send(&self.tokens, || self.http.patch(&url).json(&body)).await?;
        to_remote(&r.json::<DriveItem>().await?)
    }

    async fn trash(&self, id: &str) -> Result<()> {
        let url = format!("{BASE}/items/{id}");
        match http::send(&self.tokens, || self.http.delete(&url)).await {
            Ok(_) => Ok(()),
            Err(e) if http::status_of(&e) == Some(StatusCode::NOT_FOUND) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn quota(&self) -> Result<Quota> {
        #[derive(Deserialize)]
        struct Drive {
            quota: Option<Q>,
        }
        #[derive(Deserialize)]
        struct Q {
            used: Option<u64>,
            total: Option<u64>,
        }
        let d: Drive = self.get_json(&format!("{BASE}?$select=quota")).await?;
        let q = d.quota.context("o drive não informou cota")?;
        Ok(Quota { used: q.used.unwrap_or(0), total: q.total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_microsoft() {
        let ep = endpoints("abc".into());
        assert_eq!(ep.client_id, "abc");
        assert!(ep.client_secret.is_none());
        assert_eq!(ep.redirect_host, "localhost");
        assert_eq!(ep.scopes.join(" "), "Files.ReadWrite.All offline_access User.Read");
        assert!(ep.token_url.contains("microsoftonline"));
        assert!(ep.extra_auth_params.is_empty());
    }
}
