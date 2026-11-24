//! Testes de contrato do provedor OneDrive contra um Graph falso que devolve
//! respostas gravadas (`tests/fixtures`). Cobrem paginação do `delta`,
//! cursor invalidado, sessão de upload em partes e QuickXorHash.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use omarchy_drive_core::hash::hash_file;
use omarchy_drive_core::oauth::{OAuthEndpoints, TokenManager, TokenSet};
use omarchy_drive_core::token_store::MemoryStore;
use omarchy_drive_core::{HashKind, Provider};
use omarchy_drive_onedrive::OneDrive;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// Servidor HTTP falso
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    /// Caminho com a query, como veio na linha da requisição.
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn json_resp(status: u16, body: impl Into<Vec<u8>>) -> Resp {
    Resp { status, headers: vec![("Content-Type".into(), "application/json".into())], body: body.into() }
}

type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

struct Fake {
    url: String,
    log: Arc<Mutex<Vec<Req>>>,
}

impl Fake {
    fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 64 * 1024];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(Req { method, target, headers, body })
}

async fn serve(handler: Handler) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let handler = handler.clone();
            let log = log2.clone();
            tokio::spawn(async move {
                let Some(req) = read_request(&mut sock).await else { return };
                let resp = handler(&req);
                log.lock().unwrap().push(req);
                let mut out = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", resp.status, resp.body.len());
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.write_all(&resp.body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Fake { url, log }
}

/// Fixture com `{GRAPH}` e `{RAW}` trocados pelos endereços dos servidores falsos.
fn fixture(name: &str, graph: &str, raw: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap().replace("{GRAPH}", graph).replace("{RAW}", raw)
}

fn provider(graph: &str) -> OneDrive {
    let ep = OAuthEndpoints {
        auth_url: format!("{graph}/authorize"),
        token_url: format!("{graph}/token"),
        client_id: "teste".into(),
        client_secret: None,
        scopes: vec![],
        redirect_host: "localhost".into(),
        extra_auth_params: vec![],
    };
    let tokens = TokenSet { access_token: "TOKEN-TESTE".into(), refresh_token: "r".into(), expires_at: u64::MAX / 2 };
    OneDrive::with_graph_url(TokenManager::new(ep, Arc::new(MemoryStore::new(tokens)), "teste"), &format!("{graph}/v1.0"))
}

/// Endereço do Graph falso: o servidor ouve em `url`, a API fica em `url/v1.0`.
fn graph_base(f: &Fake) -> String {
    format!("{}/v1.0", f.url)
}

// ---------------------------------------------------------------------------
// delta
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delta_segue_next_link_ate_o_delta_link() {
    let graph_url = Arc::new(Mutex::new(String::new()));
    let g = graph_url.clone();
    let fake = serve(Arc::new(move |r: &Req| {
        let graph = g.lock().unwrap().clone();
        match r.target.as_str() {
            t if t.ends_with("token=cursor-antigo") => json_resp(200, fixture("delta_page1.json", &graph, "")),
            t if t.ends_with("token=pagina2") => json_resp(200, fixture("delta_page2.json", &graph, "")),
            _ => json_resp(404, "{}"),
        }
    }))
    .await;
    let graph = graph_base(&fake);
    *graph_url.lock().unwrap() = graph.clone();

    let od = provider(&fake.url);
    let cursor = format!("{graph}/me/drive/root/delta?token=cursor-antigo");
    let b = od.changes("ROOTID", &cursor).await.unwrap();

    assert!(!b.reset);
    assert_eq!(b.cursor, format!("{graph}/me/drive/root/delta?token=cursor-novo"));
    // Raiz fora; A1 aparece uma vez com o estado da última página.
    let ids: Vec<_> = b.upserts.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["F1", "A1", "N1"]);
    assert_eq!(b.removed, ["B9"]);

    let a1 = b.upserts.iter().find(|i| i.id == "A1").unwrap();
    assert_eq!(a1.name, "relatório final.txt");
    assert_eq!(a1.parent_id.as_deref(), Some("F1"));
    assert_eq!(a1.size, 31);
    assert_eq!(a1.hash.as_deref(), Some("MF73M+tYTpHukjCuEmQ5SBzQWhM="));
    // O rev é o cTag (muda só com o conteúdo): o rename não troca o rev.
    assert_eq!(a1.rev, "\"c:{A1},7\"");
    assert_eq!(a1.mtime_ms, 1_714_728_615_250);

    let f1 = b.upserts.iter().find(|i| i.id == "F1").unwrap();
    assert!(f1.is_dir);
    assert_eq!(f1.hash, None);
    assert!(b.upserts.iter().find(|i| i.id == "N1").unwrap().native);

    let reqs = fake.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs.iter().all(|r| r.header("authorization") == Some("Bearer TOKEN-TESTE")));
}

#[tokio::test]
async fn cursor_invalidado_com_410_vira_reset() {
    let fake = serve(Arc::new(|_: &Req| json_resp(410, fixture("resync_required.json", "", "")))).await;
    let od = provider(&fake.url);
    let cursor = format!("{}/me/drive/root/delta?token=velho", graph_base(&fake));
    let b = od.changes("ROOTID", &cursor).await.unwrap();
    assert!(b.reset);
    assert!(b.upserts.is_empty() && b.removed.is_empty());
    assert!(b.cursor.is_empty(), "o cursor antigo não pode ser reaproveitado");
    assert_eq!(fake.requests().len(), 1, "410 não deve ser repetido");
}

#[tokio::test]
async fn resync_required_em_outro_status_tambem_vira_reset() {
    let fake = serve(Arc::new(|_: &Req| json_resp(400, fixture("resync_required.json", "", "")))).await;
    let od = provider(&fake.url);
    let b = od.changes("ROOTID", &format!("{}/me/drive/root/delta?token=x", graph_base(&fake))).await.unwrap();
    assert!(b.reset);
}

#[tokio::test]
async fn outro_erro_do_delta_nao_vira_reset() {
    let fake = serve(Arc::new(|_: &Req| json_resp(403, r#"{"error":{"code":"accessDenied"}}"#))).await;
    let od = provider(&fake.url);
    let e = od.changes("ROOTID", &format!("{}/me/drive/root/delta?token=x", graph_base(&fake))).await.unwrap_err();
    assert!(format!("{e:#}").contains("403"));
}

// ---------------------------------------------------------------------------
// upload em sessão
// ---------------------------------------------------------------------------

const MIB: usize = 1024 * 1024;

#[tokio::test]
async fn upload_de_20_mb_em_partes_com_retomada() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("grande.bin");
    let total = 20 * MIB + 12_345;
    let data: Vec<u8> = (0..total).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&src, &data).unwrap();
    let local_hash = hash_file(HashKind::QuickXor, &src).unwrap();
    let mtime_ms = 1_714_728_615_000i64;

    // Servidor das URLs pré-autenticadas: guarda os bytes recebidos e falha
    // uma vez no segundo pedaço para exercitar a retomada.
    let received = Arc::new(Mutex::new(Vec::<u8>::new()));
    let failed_once = Arc::new(Mutex::new(false));
    let (rcv, fo, hash) = (received.clone(), failed_once.clone(), local_hash.clone());
    let raw = serve(Arc::new(move |r: &Req| {
        let mut got = rcv.lock().unwrap();
        if r.method == "GET" {
            return json_resp(200, format!(r#"{{"nextExpectedRanges":["{}-"]}}"#, got.len()));
        }
        let range = r.header("content-range").unwrap().trim_start_matches("bytes ").to_string();
        let (span, size) = range.split_once('/').unwrap();
        let (start, end) = span.split_once('-').unwrap();
        let (start, end, size): (usize, usize, usize) =
            (start.parse().unwrap(), end.parse().unwrap(), size.parse().unwrap());
        if start > 0 && !*fo.lock().unwrap() {
            *fo.lock().unwrap() = true;
            return json_resp(503, r#"{"error":{"code":"serviceNotAvailable"}}"#);
        }
        assert_eq!(start, got.len(), "pedaço fora de ordem");
        assert_eq!(r.body.len(), end - start + 1);
        got.extend_from_slice(&r.body);
        if got.len() < size {
            json_resp(202, format!(r#"{{"expirationDateTime":"2024-06-09T08:00:00Z","nextExpectedRanges":["{}-"]}}"#, got.len()))
        } else {
            json_resp(
                201,
                format!(
                    r#"{{"id":"U1","name":"grande.bin","size":{size},"cTag":"\"c:{{U1}},1\"",
                        "file":{{"mimeType":"application/octet-stream","hashes":{{"quickXorHash":"{hash}"}}}},
                        "parentReference":{{"id":"F1"}},
                        "fileSystemInfo":{{"lastModifiedDateTime":"2024-05-03T09:30:15Z"}}}}"#
                ),
            )
        }
    }))
    .await;

    let raw_url = raw.url.clone();
    let graph_fake = serve(Arc::new(move |r: &Req| {
        if r.method == "POST" && r.target.contains("/createUploadSession") {
            json_resp(200, fixture("upload_session.json", "", &raw_url))
        } else {
            json_resp(404, "{}")
        }
    }))
    .await;

    let od = provider(&graph_fake.url);
    let item = od.upload("F1", "grande.bin", &src, mtime_ms, None).await.unwrap();

    assert_eq!(*received.lock().unwrap(), data, "conteúdo recebido difere do enviado");
    assert_eq!(item.id, "U1");
    assert_eq!(item.size, total as u64);
    assert_eq!(item.hash.as_deref(), Some(local_hash.as_str()));

    // Criação da sessão: autenticada, no caminho do pai, com data e replace.
    let g = graph_fake.requests();
    assert_eq!(g.len(), 1, "sem PATCH extra quando a data já confere");
    assert!(g[0].target.ends_with("/v1.0/me/drive/items/F1:/grande.bin:/createUploadSession"));
    assert_eq!(g[0].header("authorization"), Some("Bearer TOKEN-TESTE"));
    let body: serde_json::Value = serde_json::from_slice(&g[0].body).unwrap();
    assert_eq!(body["item"]["@microsoft.graph.conflictBehavior"], "replace");
    assert_eq!(body["item"]["fileSystemInfo"]["lastModifiedDateTime"], "2024-05-03T09:30:15.000Z");

    // Pedaços: múltiplos de 320 KiB, sem token, com retomada após o 503.
    let r = raw.requests();
    assert!(r.iter().all(|q| q.header("authorization").is_none()), "a URL da sessão não pode receber o token");
    assert!(r.iter().all(|q| q.target.starts_with("/up/sessao-1?")));
    let puts: Vec<_> = r.iter().filter(|q| q.method == "PUT").map(|q| q.header("content-range").unwrap().to_string()).collect();
    let chunk = 10 * MIB;
    assert_eq!(
        puts,
        [
            format!("bytes 0-{}/{total}", chunk - 1),
            format!("bytes {chunk}-{}/{total}", 2 * chunk - 1), // 503
            format!("bytes {chunk}-{}/{total}", 2 * chunk - 1),
            format!("bytes {}-{}/{total}", 2 * chunk, total - 1),
        ]
    );
    assert_eq!(r.iter().filter(|q| q.method == "GET").count(), 1, "consulta a sessão uma vez após a falha");
}

#[tokio::test]
async fn upload_que_falha_cancela_a_sessao() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("x.bin");
    std::fs::write(&src, vec![1u8; 5 * MIB]).unwrap();

    let raw = serve(Arc::new(|r: &Req| match r.method.as_str() {
        "DELETE" => json_resp(204, ""),
        _ => json_resp(400, r#"{"error":{"code":"invalidRange"}}"#),
    }))
    .await;
    let raw_url = raw.url.clone();
    let graph_fake =
        serve(Arc::new(move |_: &Req| json_resp(200, fixture("upload_session.json", "", &raw_url)))).await;

    let od = provider(&graph_fake.url);
    assert!(od.upload("F1", "x.bin", &src, 0, None).await.is_err());
    let methods: Vec<_> = raw.requests().iter().map(|r| r.method.clone()).collect();
    assert_eq!(methods, ["PUT", "DELETE"]);
}

// ---------------------------------------------------------------------------
// download e QuickXorHash
// ---------------------------------------------------------------------------

#[tokio::test]
async fn download_segue_o_redirecionamento_sem_token_e_confere_o_quickxor() {
    let content = "conteúdo de teste do OneDrive\n".as_bytes();
    let cdn = serve(Arc::new(move |_: &Req| Resp {
        status: 200,
        headers: vec![("Content-Type".into(), "application/octet-stream".into())],
        body: content.to_vec(),
    }))
    .await;
    let cdn_url = cdn.url.clone();
    let graph_url = Arc::new(Mutex::new(String::new()));
    let g = graph_url.clone();
    let graph_fake = serve(Arc::new(move |r: &Req| {
        if r.target.ends_with("/items/A1/content") {
            Resp {
                status: 302,
                headers: vec![("Location".into(), format!("{cdn_url}/download/A1?tempauth=xyz"))],
                body: vec![],
            }
        } else if r.target.contains("token=pagina2") {
            json_resp(200, fixture("delta_page2.json", &g.lock().unwrap(), ""))
        } else {
            json_resp(404, "{}")
        }
    }))
    .await;
    *graph_url.lock().unwrap() = graph_base(&graph_fake);

    let od = provider(&graph_fake.url);
    // Item como veio do delta gravado.
    let b = od.changes("ROOTID", &format!("{}/me/drive/root/delta?token=pagina2", graph_base(&graph_fake))).await.unwrap();
    let a1 = b.upserts.into_iter().find(|i| i.id == "A1").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("baixado");
    od.download(&a1, &dest).await.unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), content);
    assert_eq!(hash_file(HashKind::QuickXor, &dest).unwrap(), a1.hash.unwrap());
    let c = cdn.requests();
    assert_eq!(c.len(), 1);
    assert!(c[0].header("authorization").is_none(), "o token não pode ir para o host do download");
    assert!(c[0].target.ends_with("tempauth=xyz"));
}

#[tokio::test]
async fn quickxor_do_arquivo_vazio() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("vazio");
    std::fs::write(&p, b"").unwrap();
    // Valor que o OneDrive informa para arquivos de 0 byte.
    assert_eq!(hash_file(HashKind::QuickXor, &p).unwrap(), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
}
