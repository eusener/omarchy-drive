//! Provedor Google Drive (API v3).

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use omarchy_drive_core::http::{self, HttpError};
use omarchy_drive_core::oauth::{OAuthEndpoints, TokenManager};
use omarchy_drive_core::hash::hash_bytes;
use omarchy_drive_core::{ChangeBatch, HashKind, Provider, Quota, RemoteItem, shortcut};
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{debug, warn};

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const NATIVE_PREFIX: &str = "application/vnd.google-apps.";
const FILE_FIELDS: &str = "id,name,mimeType,parents,size,modifiedTime,version,md5Checksum,trashed";
/// Tamanho do pedaço do upload resumível (múltiplo de 256 KiB).
const CHUNK: u64 = 8 * 1024 * 1024;
/// Pastas listadas em paralelo na busca em largura.
const LIST_CONCURRENCY: usize = 4;
/// Tentativas seguidas sem progresso antes de desistir de um upload.
const UPLOAD_MAX_RETRIES: u32 = 8;

/// Endpoints OAuth do Google para apps de desktop.
pub fn endpoints(client_id: String, client_secret: Option<String>) -> OAuthEndpoints {
    OAuthEndpoints {
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
        token_url: "https://oauth2.googleapis.com/token".into(),
        client_id,
        client_secret,
        scopes: vec!["https://www.googleapis.com/auth/drive".into()],
        redirect_host: "127.0.0.1".into(),
        extra_auth_params: vec![
            ("access_type".into(), "offline".into()),
            ("prompt".into(), "consent".into()),
        ],
    }
}

// ----------------------------------------------------------------------
// Modelo JSON
// ----------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DriveFile {
    id: String,
    name: String,
    mime_type: String,
    parents: Vec<String>,
    size: Option<String>,
    modified_time: Option<String>,
    version: Option<String>,
    md5_checksum: Option<String>,
    trashed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    file_id: Option<String>,
    #[serde(default)]
    removed: bool,
    file: Option<DriveFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeList {
    #[serde(default)]
    changes: Vec<Change>,
    next_page_token: Option<String>,
    new_start_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartPageToken {
    start_page_token: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ParentsOnly {
    parents: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct About {
    user: Option<AboutUser>,
    storage_quota: Option<StorageQuota>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AboutUser {
    email_address: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct StorageQuota {
    limit: Option<String>,
    usage: Option<String>,
}

/// Documentos nativos viram atalhos: nome com extensão (`.gdoc`...), e
/// tamanho e md5 do JSON do atalho, que a varredura local calcula igual.
fn map_file(f: DriveFile) -> RemoteItem {
    let is_dir = f.mime_type == FOLDER_MIME;
    let native = !is_dir && f.mime_type.starts_with(NATIVE_PREFIX);
    if native {
        let name = format!("{}.{}", f.name, shortcut::ext_for_mime(&f.mime_type));
        let bytes = shortcut::content(&f.id, &name);
        let mtime_ms = parse_mtime(f.modified_time.as_deref());
        return RemoteItem {
            parent_id: f.parents.into_iter().next(),
            size: bytes.len() as u64,
            hash: Some(hash_bytes(HashKind::Md5, &bytes)),
            id: f.id,
            name,
            is_dir: false,
            mtime_ms,
            rev: f.version.unwrap_or_default(),
            native: true,
        };
    }
    let mtime_ms = parse_mtime(f.modified_time.as_deref());
    RemoteItem {
        id: f.id,
        parent_id: f.parents.into_iter().next(),
        name: f.name,
        is_dir,
        size: f.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
        mtime_ms,
        rev: f.version.unwrap_or_default(),
        hash: f.md5_checksum,
        native,
    }
}

fn parse_mtime(t: Option<&str>) -> i64 {
    t.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp_millis()).unwrap_or(0)
}

/// Escapa um literal para uso entre aspas simples na linguagem de busca `q`.
fn escape_q(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn rfc3339_ms(ms: i64) -> Result<String> {
    let t = chrono::DateTime::from_timestamp_millis(ms).context("mtime fora do intervalo")?;
    Ok(t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Interpreta o cabeçalho `Range: bytes=0-N` de uma resposta 308: próximo
/// byte a enviar. Sem cabeçalho = nada persistido.
fn next_offset(range: Option<&str>) -> u64 {
    range
        .and_then(|r| r.trim().strip_prefix("bytes="))
        .and_then(|r| r.split('-').nth(1))
        .and_then(|end| end.trim().parse::<u64>().ok())
        .map(|end| end + 1)
        .unwrap_or(0)
}

/// O erro indica cursor (`pageToken`) inválido ou expirado?
fn is_invalid_cursor(e: &anyhow::Error) -> bool {
    let Some(h) = e.downcast_ref::<HttpError>() else { return false };
    match h.status {
        StatusCode::NOT_FOUND | StatusCode::GONE => true,
        StatusCode::BAD_REQUEST => {
            let b = h.body.to_ascii_lowercase();
            b.contains("pagetoken") || b.contains("page token")
        }
        _ => false,
    }
}

// ----------------------------------------------------------------------
// Provedor
// ----------------------------------------------------------------------

pub struct GDrive {
    tokens: TokenManager,
    http: reqwest::Client,
    /// Cliente sem redirecionamento: o 308 do upload resumível não é redirect.
    upload_http: reqwest::Client,
    root_id: tokio::sync::OnceCell<String>,
}

enum UploadStep {
    Done(RemoteItem),
    Resume(u64),
    Retry,
}

impl GDrive {
    pub fn new(tokens: TokenManager) -> Self {
        let upload_http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self { tokens, http: reqwest::Client::new(), upload_http, root_id: tokio::sync::OnceCell::new() }
    }

    /// E-mail da conta autorizada.
    pub async fn account_email(&self) -> Result<String> {
        let a: About = self.get_json(&format!("{API}/about"), &[("fields", "user(emailAddress)")]).await?;
        a.user.and_then(|u| u.email_address).context("a API não devolveu o e-mail da conta")
    }

    async fn get_json<T: DeserializeOwned>(&self, url: &str, query: &[(&str, &str)]) -> Result<T> {
        let r = http::send(&self.tokens, || {
            self.http.get(url).query(&[("supportsAllDrives", "true")]).query(query)
        })
        .await?;
        Ok(r.json().await?)
    }

    async fn get_file(&self, id: &str) -> Result<RemoteItem> {
        let f: DriveFile = self.get_json(&format!("{API}/files/{id}"), &[("fields", FILE_FIELDS)]).await?;
        Ok(map_file(f))
    }

    async fn drive_root_id(&self) -> Result<&str> {
        self.root_id
            .get_or_try_init(|| async { self.get_file("root").await.map(|it| it.id) })
            .await
            .map(String::as_str)
    }

    /// Lista todas as páginas de `files.list` para a consulta `q`.
    async fn list_query(&self, q: &str, all_drives: bool) -> Result<Vec<RemoteItem>> {
        let fields = format!("nextPageToken,files({FILE_FIELDS})");
        let mut out = Vec::new();
        let mut page: Option<String> = None;
        loop {
            let mut query = vec![("q", q), ("pageSize", "1000"), ("fields", fields.as_str())];
            if all_drives {
                query.push(("includeItemsFromAllDrives", "true"));
            }
            if let Some(p) = &page {
                query.push(("pageToken", p.as_str()));
            }
            let l: FileList = self.get_json(&format!("{API}/files"), &query).await?;
            out.extend(l.files.into_iter().filter(|f| !f.trashed).map(map_file));
            match l.next_page_token {
                Some(p) => page = Some(p),
                None => return Ok(out),
            }
        }
    }

    async fn list_children(&self, id: String) -> Result<Vec<RemoteItem>> {
        let q = format!("'{}' in parents and trashed = false", escape_q(&id));
        self.list_query(&q, true).await
    }

    async fn find_child_folder(&self, parent: &str, name: &str) -> Result<Option<RemoteItem>> {
        let q = format!(
            "'{}' in parents and name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false",
            escape_q(parent),
            escape_q(name)
        );
        Ok(self.list_query(&q, true).await?.into_iter().next())
    }

    /// Inicia uma sessão de upload resumível e devolve a URL da sessão.
    async fn start_session(
        &self,
        parent_id: &str,
        name: &str,
        mtime_ms: i64,
        total: u64,
        existing_id: Option<&str>,
    ) -> Result<String> {
        let modified = rfc3339_ms(mtime_ms)?;
        let query = [("uploadType", "resumable"), ("supportsAllDrives", "true"), ("fields", FILE_FIELDS)];
        let total_s = total.to_string();
        let resp = match existing_id {
            None => {
                let url = format!("{UPLOAD_API}/files");
                let body = json!({ "name": name, "parents": [parent_id], "modifiedTime": modified });
                http::send(&self.tokens, || {
                    self.http
                        .post(&url)
                        .query(&query)
                        .header("X-Upload-Content-Length", &total_s)
                        .json(&body)
                })
                .await?
            }
            Some(id) => {
                let url = format!("{UPLOAD_API}/files/{id}");
                let body = json!({ "modifiedTime": modified });
                http::send(&self.tokens, || {
                    self.http
                        .patch(&url)
                        .query(&query)
                        .header("X-Upload-Content-Length", &total_s)
                        .json(&body)
                })
                .await?
            }
        };
        let loc = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .context("upload resumível sem cabeçalho Location")?;
        Ok(loc.to_string())
    }

    /// Envia o pedaço que começa em `offset` (ou consulta o progresso, se
    /// `query_only`).
    async fn put_chunk(
        &self,
        session: &str,
        file: &mut tokio::fs::File,
        offset: u64,
        total: u64,
        query_only: bool,
    ) -> Result<std::result::Result<reqwest::Response, reqwest::Error>> {
        let token = self.tokens.access_token().await?;
        let req = self.upload_http.put(session).bearer_auth(token);
        let req = if total == 0 {
            req.header("Content-Range", "bytes */0").header("Content-Length", "0").body(Vec::<u8>::new())
        } else if query_only {
            req.header("Content-Range", format!("bytes */{total}"))
                .header("Content-Length", "0")
                .body(Vec::<u8>::new())
        } else {
            let end = (offset + CHUNK).min(total);
            let mut buf = vec![0u8; (end - offset) as usize];
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            file.read_exact(&mut buf).await.context("arquivo encolheu durante o upload")?;
            req.header("Content-Range", format!("bytes {offset}-{}/{total}", end - 1)).body(buf)
        };
        Ok(req.send().await)
    }

    async fn classify(&self, res: std::result::Result<reqwest::Response, reqwest::Error>) -> Result<UploadStep> {
        let r = match res {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() || e.is_request() || e.is_body() => {
                debug!("falha de rede no upload: {e}");
                return Ok(UploadStep::Retry);
            }
            Err(e) => return Err(e.into()),
        };
        let status = r.status();
        match status.as_u16() {
            200 | 201 => {
                let f: DriveFile = r.json().await?;
                Ok(UploadStep::Done(map_file(f)))
            }
            308 => {
                let range = r.headers().get(reqwest::header::RANGE).and_then(|v| v.to_str().ok());
                Ok(UploadStep::Resume(next_offset(range)))
            }
            401 => {
                self.tokens.invalidate().await;
                Ok(UploadStep::Retry)
            }
            429 | 500..=599 => Ok(UploadStep::Retry),
            _ => {
                let body = r.text().await.unwrap_or_default();
                if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                    bail!("sessão de upload expirou ({status}); será refeito no próximo ciclo");
                }
                Err(HttpError { status, body }.into())
            }
        }
    }
}

#[async_trait]
impl Provider for GDrive {
    fn name(&self) -> &'static str {
        "gdrive"
    }

    fn hash_kind(&self) -> HashKind {
        HashKind::Md5
    }

    async fn resolve_folder(&self, path: &str) -> Result<RemoteItem> {
        let root = self.get_file("root").await?;
        let _ = self.root_id.set(root.id.clone());
        let mut cur = root;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            cur = match self.find_child_folder(&cur.id, seg).await? {
                Some(f) => f,
                None => {
                    debug!("criando pasta remota {seg:?}");
                    self.create_folder(&cur.id, seg).await?
                }
            };
        }
        Ok(cur)
    }

    async fn start_cursor(&self, _root_id: &str) -> Result<String> {
        let t: StartPageToken = self.get_json(&format!("{API}/changes/startPageToken"), &[]).await?;
        Ok(t.start_page_token)
    }

    async fn list_tree(&self, root_id: &str) -> Result<Vec<RemoteItem>> {
        if root_id == "root" || root_id == self.drive_root_id().await? {
            // Raiz do drive: uma listagem plana é bem mais barata; o motor
            // filtra pela ancestralidade.
            return self.list_query("trashed = false", false).await;
        }
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::from([root_id.to_string()]);
        let mut level = vec![root_id.to_string()];
        while !level.is_empty() {
            let results: Vec<Vec<RemoteItem>> = futures::stream::iter(level.drain(..).map(|id| self.list_children(id)))
                .buffer_unordered(LIST_CONCURRENCY)
                .try_collect()
                .await?;
            for it in results.into_iter().flatten() {
                if it.is_dir && seen.insert(it.id.clone()) {
                    level.push(it.id.clone());
                }
                out.push(it);
            }
        }
        Ok(out)
    }

    async fn changes(&self, _root_id: &str, cursor: &str) -> Result<ChangeBatch> {
        let fields = format!("nextPageToken,newStartPageToken,changes(fileId,removed,file({FILE_FIELDS}))");
        let mut batch = ChangeBatch::default();
        let mut page = cursor.to_string();
        loop {
            let query = [
                ("pageToken", page.as_str()),
                ("pageSize", "1000"),
                ("includeRemoved", "true"),
                ("restrictToMyDrive", "false"),
                ("includeItemsFromAllDrives", "true"),
                ("fields", fields.as_str()),
            ];
            let l: ChangeList = match self.get_json(&format!("{API}/changes"), &query).await {
                Ok(l) => l,
                Err(e) if is_invalid_cursor(&e) => {
                    warn!("cursor de mudanças inválido, relistando: {e}");
                    return Ok(ChangeBatch { reset: true, ..Default::default() });
                }
                Err(e) => return Err(e),
            };
            for c in l.changes {
                let Some(id) = c.file_id else { continue };
                match c.file {
                    Some(f) if !c.removed && !f.trashed => batch.upserts.push(map_file(f)),
                    _ => batch.removed.push(id),
                }
            }
            if let Some(next) = l.next_page_token {
                page = next;
            } else {
                batch.cursor = l.new_start_page_token.context("changes.list sem newStartPageToken")?;
                return Ok(batch);
            }
        }
    }

    async fn download(&self, item: &RemoteItem, dest: &Path) -> Result<()> {
        if item.native {
            // Atalho: gerado localmente, sem tocar a rede.
            tokio::fs::write(dest, shortcut::content(&item.id, &item.name)).await?;
            return Ok(());
        }
        let url = format!("{API}/files/{}", item.id);
        let resp = http::send(&self.tokens, || {
            self.http.get(&url).query(&[("alt", "media"), ("supportsAllDrives", "true")])
        })
        .await?;
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = resp.bytes_stream();
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
        let mut file = tokio::fs::File::open(src).await?;
        let total = file.metadata().await?.len();
        let session = self.start_session(parent_id, name, mtime_ms, total, existing_id).await?;

        let mut offset = 0u64;
        let mut query_only = false;
        let mut retries = 0u32;
        let mut delay = Duration::from_secs(1);
        loop {
            let res = self.put_chunk(&session, &mut file, offset, total, query_only).await?;
            match self.classify(res).await? {
                UploadStep::Done(item) => return Ok(item),
                UploadStep::Resume(next) => {
                    if next > total {
                        bail!("servidor confirmou mais bytes ({next}) que o arquivo tem ({total})");
                    }
                    if !query_only && next <= offset {
                        // Pedaço enviado sem progresso: conta como falha.
                        retries += 1;
                        if retries > UPLOAD_MAX_RETRIES {
                            bail!("upload de {name:?} não progride");
                        }
                    } else {
                        retries = 0;
                        delay = Duration::from_secs(1);
                    }
                    offset = next;
                    query_only = false;
                }
                UploadStep::Retry => {
                    retries += 1;
                    if retries > UPLOAD_MAX_RETRIES {
                        bail!("upload de {name:?} falhou após {UPLOAD_MAX_RETRIES} tentativas");
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(32));
                    // Descobre quanto o servidor já guardou antes de continuar.
                    query_only = true;
                }
            }
        }
    }

    async fn create_folder(&self, parent_id: &str, name: &str) -> Result<RemoteItem> {
        let url = format!("{API}/files");
        let body = json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent_id] });
        let r = http::send(&self.tokens, || {
            self.http
                .post(&url)
                .query(&[("supportsAllDrives", "true"), ("fields", FILE_FIELDS)])
                .json(&body)
        })
        .await?;
        Ok(map_file(r.json().await?))
    }

    async fn move_item(&self, id: &str, new_parent_id: &str, new_name: &str) -> Result<RemoteItem> {
        let url = format!("{API}/files/{id}");
        let cur: ParentsOnly = self.get_json(&url, &[("fields", "parents")]).await?;
        let mut query = vec![("supportsAllDrives", "true".to_string()), ("fields", FILE_FIELDS.to_string())];
        if !cur.parents.iter().any(|p| p == new_parent_id) {
            query.push(("addParents", new_parent_id.to_string()));
            if !cur.parents.is_empty() {
                query.push(("removeParents", cur.parents.join(",")));
            }
        }
        let body = json!({ "name": new_name });
        let r = http::send(&self.tokens, || self.http.patch(&url).query(&query).json(&body)).await?;
        Ok(map_file(r.json().await?))
    }

    async fn trash(&self, id: &str) -> Result<()> {
        let url = format!("{API}/files/{id}");
        let body = json!({ "trashed": true });
        let res = http::send(&self.tokens, || {
            self.http
                .patch(&url)
                .query(&[("supportsAllDrives", "true"), ("fields", "id")])
                .json(&body)
        })
        .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) if http::status_of(&e) == Some(StatusCode::NOT_FOUND) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn quota(&self) -> Result<Quota> {
        let a: About = self.get_json(&format!("{API}/about"), &[("fields", "storageQuota(limit,usage)")]).await?;
        let q = a.storage_quota.unwrap_or_default();
        Ok(Quota {
            used: q.usage.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
            total: q.limit.as_deref().and_then(|s| s.parse().ok()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: serde_json::Value) -> RemoteItem {
        map_file(serde_json::from_value(v).unwrap())
    }

    #[test]
    fn mapeia_pasta() {
        let it = parse(json!({
            "id": "F1", "name": "Docs", "mimeType": FOLDER_MIME,
            "parents": ["ROOT", "OUTRO"], "modifiedTime": "2024-01-02T03:04:05.678Z", "version": "12"
        }));
        assert!(it.is_dir);
        assert!(!it.native);
        assert_eq!(it.parent_id.as_deref(), Some("ROOT"));
        assert_eq!(it.size, 0);
        assert_eq!(it.rev, "12");
        assert_eq!(it.hash, None);
        assert_eq!(it.mtime_ms, 1_704_164_645_678);
    }

    #[test]
    fn mapeia_arquivo_comum() {
        let it = parse(json!({
            "id": "A1", "name": "nota.txt", "mimeType": "text/plain", "parents": ["F1"],
            "size": "1234", "modifiedTime": "2024-01-02T03:04:05+00:00", "version": "7",
            "md5Checksum": "d41d8cd98f00b204e9800998ecf8427e", "trashed": false
        }));
        assert_eq!(
            it,
            RemoteItem {
                id: "A1".into(),
                parent_id: Some("F1".into()),
                name: "nota.txt".into(),
                is_dir: false,
                size: 1234,
                mtime_ms: 1_704_164_645_000,
                rev: "7".into(),
                hash: Some("d41d8cd98f00b204e9800998ecf8427e".into()),
                native: false,
            }
        );
    }

    #[test]
    fn mapeia_documento_nativo() {
        let it = parse(json!({
            "id": "G1", "name": "Planilha", "mimeType": "application/vnd.google-apps.spreadsheet",
            "parents": ["F1"], "modifiedTime": "2024-01-02T03:04:05Z", "version": "3"
        }));
        assert!(it.native);
        assert!(!it.is_dir);
        assert_eq!(it.name, "Planilha.gsheet");
        let bytes = shortcut::content("G1", "Planilha.gsheet");
        assert_eq!(it.size, bytes.len() as u64);
        assert_eq!(it.hash, Some(hash_bytes(HashKind::Md5, &bytes)));
        assert!(it.size < 1024);
    }

    #[test]
    fn atalhos_por_tipo() {
        for (mime, name) in [
            ("application/vnd.google-apps.document", "Relatório.gdoc"),
            ("application/vnd.google-apps.presentation", "Relatório.gslides"),
            ("application/vnd.google-apps.jam", "Relatório.glink"),
        ] {
            let it = parse(json!({ "id": "D", "name": "Relatório", "mimeType": mime }));
            assert_eq!(it.name, name);
            assert!(shortcut::url_from(&shortcut::content(&it.id, &it.name)).is_some());
        }
    }

    #[test]
    fn campos_ausentes() {
        let it = parse(json!({ "id": "X", "name": "x.bin", "mimeType": "application/octet-stream" }));
        assert_eq!(it.parent_id, None);
        assert_eq!(it.size, 0);
        assert_eq!(it.mtime_ms, 0);
        assert_eq!(it.rev, "");
        assert_eq!(it.hash, None);
        assert!(!it.native && !it.is_dir);
    }

    #[test]
    fn mudancas_e_lista() {
        let l: ChangeList = serde_json::from_value(json!({
            "newStartPageToken": "99",
            "changes": [
                { "fileId": "A", "removed": true },
                { "fileId": "B", "removed": false, "file": { "id": "B", "name": "b", "mimeType": "text/plain", "trashed": true } },
                { "removed": false }
            ]
        }))
        .unwrap();
        assert_eq!(l.new_start_page_token.as_deref(), Some("99"));
        assert!(l.next_page_token.is_none());
        assert_eq!(l.changes.len(), 3);
        assert!(l.changes[1].file.as_ref().unwrap().trashed);
    }

    #[test]
    fn escapa_consulta() {
        assert_eq!(escape_q("simples"), "simples");
        assert_eq!(escape_q("d'água"), "d\\'água");
        assert_eq!(escape_q("a\\b"), "a\\\\b");
        assert_eq!(escape_q("\\'"), "\\\\\\'");
    }

    #[test]
    fn offset_do_range() {
        assert_eq!(next_offset(None), 0);
        assert_eq!(next_offset(Some("bytes=0-8388607")), 8_388_608);
        assert_eq!(next_offset(Some("lixo")), 0);
    }

    #[test]
    fn cursor_invalido() {
        let e: anyhow::Error = HttpError { status: StatusCode::GONE, body: String::new() }.into();
        assert!(is_invalid_cursor(&e));
        let e: anyhow::Error =
            HttpError { status: StatusCode::BAD_REQUEST, body: "Invalid Value: pageToken".into() }.into();
        assert!(is_invalid_cursor(&e));
        let e: anyhow::Error = HttpError { status: StatusCode::BAD_REQUEST, body: "outra coisa".into() }.into();
        assert!(!is_invalid_cursor(&e));
    }

    #[test]
    fn mtime_rfc3339() {
        assert_eq!(rfc3339_ms(1_704_164_645_678).unwrap(), "2024-01-02T03:04:05.678Z");
    }
}
