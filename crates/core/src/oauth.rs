//! OAuth 2.0 para apps de desktop: Authorization Code + PKCE com redirect
//! loopback (RFC 8252), usado por Google e Microsoft.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    /// `127.0.0.1` (Google) ou `localhost` (Microsoft).
    pub redirect_host: String,
    pub extra_auth_params: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// Segundos Unix.
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_b64(n: usize) -> Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// Abre o navegador para o consentimento e espera o redirect no loopback.
pub async fn authorize_interactive(ep: &OAuthEndpoints) -> Result<TokenSet> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://{}:{port}", ep.redirect_host);
    let verifier = random_b64(48)?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16)?;

    let mut url = url::Url::parse(&ep.auth_url)?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &ep.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("response_type", "code")
            .append_pair("scope", &ep.scopes.join(" "))
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        for (k, v) in &ep.extra_auth_params {
            q.append_pair(k, v);
        }
    }

    eprintln!("Abrindo o navegador para autorizar o acesso. Se não abrir, acesse:\n\n{url}\n");
    let _ = std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    let code = loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await?;
        let req = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = req.lines().next().and_then(|l| l.split_whitespace().nth(1)) else {
            continue;
        };
        let parsed = url::Url::parse(&format!("http://localhost{target}"))?;
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        if params.is_empty() {
            // favicon etc.
            let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
            continue;
        }
        let (ok, body) = match (params.get("code"), params.get("state"), params.get("error")) {
            (Some(c), Some(s), None) if *s == state => (Some(c.clone()), "Autorizado! Pode fechar esta aba e voltar ao terminal."),
            (_, _, Some(_)) => (None, "Autorização negada. Pode fechar esta aba."),
            _ => (None, "Resposta inválida (state não confere)."),
        };
        let html = format!(
            "<!doctype html><meta charset=utf-8><title>omarchy-drive</title><body style=\"font-family:sans-serif;padding:3em\"><h2>omarchy-drive</h2><p>{body}</p>"
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        match ok {
            Some(c) => break c,
            None => bail!("{body}"),
        }
    };

    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect),
        ("client_id", ep.client_id.clone()),
        ("code_verifier", verifier),
    ];
    if let Some(s) = &ep.client_secret {
        form.push(("client_secret", s.clone()));
    }
    let resp = reqwest::Client::new().post(&ep.token_url).form(&form).send().await?;
    if !resp.status().is_success() {
        bail!("troca do código falhou: {}", resp.text().await.unwrap_or_default());
    }
    let t: TokenResponse = resp.json().await?;
    Ok(TokenSet {
        access_token: t.access_token,
        refresh_token: t.refresh_token.context("o provedor não devolveu refresh_token")?,
        expires_at: now() + t.expires_in.unwrap_or(3600),
    })
}

pub fn save_tokens(path: &std::path::Path, t: &TokenSet) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(t)?)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Mantém o access token válido, renovando e persistindo quando necessário.
pub struct TokenManager {
    ep: OAuthEndpoints,
    path: PathBuf,
    http: reqwest::Client,
    tokens: tokio::sync::Mutex<TokenSet>,
}

impl TokenManager {
    pub fn load(ep: OAuthEndpoints, path: PathBuf) -> Result<Self> {
        let raw = std::fs::read(&path).with_context(|| {
            format!("conta não autorizada ({}). Rode `omarchy-drive account add`", path.display())
        })?;
        let tokens: TokenSet = serde_json::from_slice(&raw)?;
        Ok(Self { ep, path, http: reqwest::Client::new(), tokens: tokio::sync::Mutex::new(tokens) })
    }

    pub async fn access_token(&self) -> Result<String> {
        let mut t = self.tokens.lock().await;
        if t.expires_at > now() + 60 {
            return Ok(t.access_token.clone());
        }
        self.refresh_locked(&mut t).await?;
        Ok(t.access_token.clone())
    }

    /// Força renovação (ex.: após um 401).
    pub async fn invalidate(&self) {
        self.tokens.lock().await.expires_at = 0;
    }

    async fn refresh_locked(&self, t: &mut TokenSet) -> Result<()> {
        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", t.refresh_token.clone()),
            ("client_id", self.ep.client_id.clone()),
        ];
        if let Some(s) = &self.ep.client_secret {
            form.push(("client_secret", s.clone()));
        }
        if self.ep.token_url.contains("microsoftonline") {
            form.push(("scope", self.ep.scopes.join(" ")));
        }
        let resp = self.http.post(&self.ep.token_url).form(&form).send().await?;
        if !resp.status().is_success() {
            bail!(
                "renovação do token falhou (reautorize com `omarchy-drive account add`): {}",
                resp.text().await.unwrap_or_default()
            );
        }
        let r: TokenResponse = resp.json().await?;
        t.access_token = r.access_token;
        // A Microsoft rotaciona o refresh token; o Google normalmente não.
        if let Some(rt) = r.refresh_token {
            t.refresh_token = rt;
        }
        t.expires_at = now() + r.expires_in.unwrap_or(3600);
        save_tokens(&self.path, t)
    }
}
