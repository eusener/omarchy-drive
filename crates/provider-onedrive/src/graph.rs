//! Tipos JSON do Microsoft Graph e conversão para o modelo do núcleo.
//! Tudo aqui é puro (sem rede) para poder ser testado em unidade.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use omarchy_drive_core::{ChangeBatch, RemoteItem};
use serde::Deserialize;
use serde_json::Value;

/// Campos pedidos via `$select` (vale também para `delta`, e os links
/// seguintes herdam a seleção).
pub const SELECT: &str = "id,name,size,folder,file,package,root,deleted,remoteItem,\
parentReference,fileSystemInfo,lastModifiedDateTime,cTag,eTag";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveItem {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub folder: Option<Value>,
    #[serde(default)]
    pub file: Option<FileFacet>,
    #[serde(default)]
    pub package: Option<Value>,
    #[serde(default)]
    pub root: Option<Value>,
    #[serde(default)]
    pub deleted: Option<Value>,
    #[serde(default)]
    pub remote_item: Option<Value>,
    #[serde(default)]
    pub parent_reference: Option<ParentReference>,
    #[serde(default)]
    pub file_system_info: Option<FileSystemInfo>,
    #[serde(default)]
    pub last_modified_date_time: Option<String>,
    #[serde(default)]
    pub c_tag: Option<String>,
    #[serde(default)]
    pub e_tag: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileFacet {
    #[serde(default)]
    pub hashes: Option<Hashes>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hashes {
    #[serde(default)]
    pub quick_xor_hash: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ParentReference {
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemInfo {
    #[serde(default)]
    pub last_modified_date_time: Option<String>,
}

/// Página de `children` ou `delta`.
#[derive(Debug, Default, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink", default)]
    pub next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink", default)]
    pub delta_link: Option<String>,
}

/// Próximo passo depois de uma página de `delta`.
#[derive(Debug, PartialEq, Eq)]
pub enum PageNext {
    /// Há mais páginas: buscar esta URL.
    More(String),
    /// Fim da enumeração: este é o novo cursor.
    Done(String),
}

impl Page {
    pub fn next(&self) -> Result<PageNext> {
        if let Some(n) = &self.next_link {
            Ok(PageNext::More(n.clone()))
        } else if let Some(d) = &self.delta_link {
            Ok(PageNext::Done(d.clone()))
        } else {
            anyhow::bail!("resposta de delta sem @odata.nextLink nem @odata.deltaLink")
        }
    }
}

impl DriveItem {
    pub fn is_deleted(&self) -> bool {
        self.deleted.is_some()
    }

    pub fn is_root(&self) -> bool {
        self.root.is_some()
    }
}

pub fn parse_time_ms(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

pub fn format_time_ms(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Converte um `driveItem` (não apagado) em [`RemoteItem`].
pub fn to_remote(it: &DriveItem) -> Result<RemoteItem> {
    let name = it.name.clone().with_context(|| format!("item {} sem nome", it.id))?;
    // Pacotes (blocos de anotações do OneNote etc.) e atalhos para itens de
    // outros drives (`remoteItem`) não têm conteúdo sincronizável aqui:
    // viram "nativos" e o motor os ignora.
    let native = it.package.is_some() || it.remote_item.is_some();
    let is_dir = it.folder.is_some() && !native;
    let mtime_ms = it
        .file_system_info
        .as_ref()
        .and_then(|f| f.last_modified_date_time.as_deref())
        .and_then(parse_time_ms)
        .or_else(|| it.last_modified_date_time.as_deref().and_then(parse_time_ms))
        .unwrap_or(0);
    let hash = if is_dir {
        None
    } else {
        it.file
            .as_ref()
            .and_then(|f| f.hashes.as_ref())
            .and_then(|h| h.quick_xor_hash.clone())
    };
    Ok(RemoteItem {
        id: it.id.clone(),
        parent_id: it.parent_reference.as_ref().and_then(|p| p.id.clone()),
        name,
        is_dir,
        size: it.size.unwrap_or(0),
        mtime_ms,
        rev: it.c_tag.clone().or_else(|| it.e_tag.clone()).unwrap_or_default(),
        hash,
        native,
    })
}

/// Acumula páginas de `delta` num [`ChangeBatch`]. Se o mesmo id aparecer
/// várias vezes, vale o último estado (apagado ou atualizado).
#[derive(Default)]
pub struct DeltaAccumulator {
    order: Vec<String>,
    state: HashMap<String, Option<RemoteItem>>,
}

impl DeltaAccumulator {
    pub fn add_page(&mut self, page: &Page) -> Result<()> {
        for it in &page.value {
            if it.is_root() {
                continue;
            }
            let entry = if it.is_deleted() { None } else { Some(to_remote(it)?) };
            if self.state.insert(it.id.clone(), entry).is_none() {
                self.order.push(it.id.clone());
            }
        }
        Ok(())
    }

    pub fn finish(mut self, cursor: String) -> ChangeBatch {
        let mut batch = ChangeBatch { cursor, ..Default::default() };
        for id in self.order {
            match self.state.remove(&id) {
                Some(Some(item)) => batch.upserts.push(item),
                Some(None) => batch.removed.push(id),
                None => {}
            }
        }
        batch
    }
}

/// Itens vivos de uma página (sem a raiz e sem apagados) — usado na
/// listagem completa.
pub fn live_items(page: &Page) -> Result<Vec<RemoteItem>> {
    page.value
        .iter()
        .filter(|it| !it.is_root() && !it.is_deleted())
        .map(to_remote)
        .collect()
}

/// Percent-encoding de um segmento de caminho: só os caracteres não
/// reservados (RFC 3986) passam crus.
pub fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Codifica um caminho relativo (`a/b c`) segmento a segmento, ignorando
/// barras repetidas/iniciais/finais.
pub fn encode_path(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Erro do Graph indica que o cursor de delta precisa ser descartado?
pub fn is_resync_body(body: &str) -> bool {
    body.contains("resyncRequired")
        || body.contains("resyncChangesApplyDifferences")
        || body.contains("resyncChangesUploadDifferences")
        || body.contains("resyncChangesUploadAll")
}

/// Início de `nextExpectedRanges` (ex.: `["12345-"]` → 12345).
pub fn next_expected_start(v: &Value) -> Option<u64> {
    let first = v.get("nextExpectedRanges")?.as_array()?.first()?.as_str()?;
    first.split('-').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(v: Value) -> DriveItem {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn pasta() {
        let it = item(json!({
            "id": "F1", "name": "Documentos", "size": 1234,
            "folder": {"childCount": 3},
            "parentReference": {"id": "ROOT", "driveId": "d"},
            "fileSystemInfo": {"lastModifiedDateTime": "2024-05-01T10:00:00Z"},
            "lastModifiedDateTime": "2024-06-01T10:00:00Z",
            "eTag": "\"{E},1\""
        }));
        let r = to_remote(&it).unwrap();
        assert!(r.is_dir);
        assert!(!r.native);
        assert_eq!(r.parent_id.as_deref(), Some("ROOT"));
        assert_eq!(r.name, "Documentos");
        assert_eq!(r.mtime_ms, 1_714_557_600_000);
        assert_eq!(r.rev, "\"{E},1\"");
        assert_eq!(r.hash, None);
    }

    #[test]
    fn arquivo_com_quickxor() {
        let it = item(json!({
            "id": "A1", "name": "nota.txt", "size": 42,
            "file": {"mimeType": "text/plain", "hashes": {"quickXorHash": "AAAAAAAAAAAAAAAAAAAAAAAAAAA=", "sha1Hash": "X"}},
            "parentReference": {"id": "F1"},
            "lastModifiedDateTime": "2024-05-01T10:00:00.5+02:00",
            "cTag": "\"c:{A},2\"", "eTag": "\"{A},3\""
        }));
        let r = to_remote(&it).unwrap();
        assert!(!r.is_dir);
        assert!(!r.native);
        assert_eq!(r.size, 42);
        assert_eq!(r.hash.as_deref(), Some("AAAAAAAAAAAAAAAAAAAAAAAAAAA="));
        assert_eq!(r.rev, "\"c:{A},2\"");
        assert_eq!(r.mtime_ms, 1_714_550_400_500);
    }

    #[test]
    fn pacote_onenote_e_nativo() {
        let it = item(json!({
            "id": "N1", "name": "Caderno", "size": 10,
            "package": {"type": "oneNote"},
            "folder": {"childCount": 1},
            "parentReference": {"id": "ROOT"}
        }));
        let r = to_remote(&it).unwrap();
        assert!(r.native);
        assert!(!r.is_dir);
    }

    #[test]
    fn delta_com_apagado_e_raiz() {
        let page: Page = serde_json::from_value(json!({
            "value": [
                {"id": "ROOT", "name": "root", "root": {}, "folder": {}},
                {"id": "A1", "name": "a.txt", "file": {}, "parentReference": {"id": "ROOT"}},
                {"id": "D1", "deleted": {"state": "deleted"}, "parentReference": {"id": "ROOT"}},
                {"id": "A2", "name": "b.txt", "file": {}, "parentReference": {"id": "ROOT"}},
                {"id": "A2", "deleted": {}}
            ],
            "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/drive/root/delta?token=abc"
        }))
        .unwrap();
        let mut acc = DeltaAccumulator::default();
        acc.add_page(&page).unwrap();
        let PageNext::Done(cursor) = page.next().unwrap() else { panic!() };
        let b = acc.finish(cursor);
        assert_eq!(b.upserts.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["A1"]);
        assert_eq!(b.removed, ["D1", "A2"]);
        assert!(!b.reset);
        assert!(b.cursor.ends_with("token=abc"));

        let live = live_items(&page).unwrap();
        assert_eq!(live.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["A1", "A2"]);
    }

    #[test]
    fn delta_restaurado_depois_de_apagado() {
        let p1: Page = serde_json::from_value(json!({
            "value": [{"id": "X", "deleted": {}}],
            "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/drive/root/delta?token=p2"
        }))
        .unwrap();
        let p2: Page = serde_json::from_value(json!({
            "value": [{"id": "X", "name": "x", "file": {}}],
            "@odata.deltaLink": "https://graph.microsoft.com/v1.0/me/drive/root/delta?token=end"
        }))
        .unwrap();
        assert_eq!(
            p1.next().unwrap(),
            PageNext::More("https://graph.microsoft.com/v1.0/me/drive/root/delta?token=p2".into())
        );
        let mut acc = DeltaAccumulator::default();
        acc.add_page(&p1).unwrap();
        acc.add_page(&p2).unwrap();
        let b = acc.finish("c".into());
        assert!(b.removed.is_empty());
        assert_eq!(b.upserts.len(), 1);
    }

    #[test]
    fn pagina_sem_links_e_erro() {
        let p: Page = serde_json::from_value(json!({"value": []})).unwrap();
        assert!(p.next().is_err());
    }

    #[test]
    fn next_link_tem_prioridade() {
        let p: Page = serde_json::from_value(json!({
            "value": [], "@odata.nextLink": "n", "@odata.deltaLink": "d"
        }))
        .unwrap();
        assert_eq!(p.next().unwrap(), PageNext::More("n".into()));
    }

    #[test]
    fn codificacao_de_segmentos() {
        assert_eq!(encode_segment("a b#c?d%e"), "a%20b%23c%3Fd%25e");
        assert_eq!(encode_segment("ação.txt"), "a%C3%A7%C3%A3o.txt");
        assert_eq!(encode_segment("x+y&z'(1)"), "x%2By%26z%27%281%29");
        assert_eq!(encode_path("/Docs//Meu Arquivo/"), "Docs/Meu%20Arquivo");
        assert_eq!(encode_path("a/b"), "a/b");
    }

    #[test]
    fn faixas_esperadas() {
        assert_eq!(next_expected_start(&json!({"nextExpectedRanges": ["327680-"]})), Some(327680));
        assert_eq!(next_expected_start(&json!({"nextExpectedRanges": ["10-20", "30-"]})), Some(10));
        assert_eq!(next_expected_start(&json!({})), None);
    }

    #[test]
    fn tempo_ida_e_volta() {
        let s = format_time_ms(1_714_557_600_123);
        assert_eq!(s, "2024-05-01T10:00:00.123Z");
        assert_eq!(parse_time_ms(&s), Some(1_714_557_600_123));
    }

    #[test]
    fn resync() {
        assert!(is_resync_body(r#"{"error":{"code":"resyncRequired"}}"#));
        assert!(!is_resync_body(r#"{"error":{"code":"itemNotFound"}}"#));
    }
}
