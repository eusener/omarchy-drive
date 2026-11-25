//! Onde ficam os tokens OAuth de cada conta.
//!
//! - [`KeyringStore`]: Secret Service (gnome-keyring, KWallet...), um item por
//!   conta com os atributos `application=omarchy-drive` e `account=<nome>`.
//!   Na primeira leitura de uma conta que ainda tem arquivo, migra o arquivo
//!   para o keyring e só apaga o arquivo depois de reler o item gravado.
//! - [`FileStore`]: arquivos `tokens/<conta>.json` com permissão 0600 numa
//!   pasta 0700; fallback quando não há Secret Service na sessão.
//!
//! Keyring bloqueado nunca cai para arquivo nem abre o prompt de senha: as
//! operações falham com [`KeyringLocked`] e o daemon tenta de novo depois.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tracing::{info, warn};

use crate::oauth::TokenSet;

const APP_ATTR: &str = "omarchy-drive";

#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn load(&self, account: &str) -> Result<TokenSet>;
    async fn save(&self, account: &str, tokens: &TokenSet) -> Result<()>;
    /// Apaga os tokens da conta (sem erro se não existirem).
    async fn delete(&self, account: &str) -> Result<()>;
    /// `keyring` ou `arquivo`, para mensagens.
    fn kind(&self) -> &'static str;
}

/// O keyring existe mas está bloqueado.
#[derive(Debug)]
pub struct KeyringLocked;

impl std::fmt::Display for KeyringLocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "keyring bloqueado: desbloqueie o chaveiro da sessão para a sincronização continuar",
        )
    }
}

impl std::error::Error for KeyringLocked {}

pub fn is_keyring_locked(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<KeyringLocked>().is_some())
}

fn not_authorized(account: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "conta {account} não autorizada. Conecte pelo painel ou rode `omarchy-drive account reauth {account}`"
    )
}

// ---------------------------------------------------------------------------

pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn path(&self, account: &str) -> PathBuf {
        self.dir.join(format!("{account}.json"))
    }

    fn read(&self, account: &str) -> Result<Option<TokenSet>> {
        match std::fs::read(self.path(account)) {
            Ok(b) => Ok(Some(
                serde_json::from_slice(&b).context("arquivo de token inválido")?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, account: &str, t: &TokenSet) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let path = self.path(account);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(t)?)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    fn remove(&self, account: &str) -> Result<()> {
        match std::fs::remove_file(self.path(account)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[async_trait]
impl TokenStore for FileStore {
    async fn load(&self, account: &str) -> Result<TokenSet> {
        self.read(account)?.ok_or_else(|| not_authorized(account))
    }
    async fn save(&self, account: &str, tokens: &TokenSet) -> Result<()> {
        self.write(account, tokens)
    }
    async fn delete(&self, account: &str) -> Result<()> {
        self.remove(account)
    }
    fn kind(&self) -> &'static str {
        "arquivo"
    }
}

// ---------------------------------------------------------------------------

pub struct KeyringStore {
    service: oo7::dbus::Service,
    /// Arquivos antigos, migrados na primeira leitura.
    legacy: FileStore,
}

impl KeyringStore {
    async fn collection(&self) -> Result<oo7::dbus::Collection> {
        let c = self
            .service
            .default_collection()
            .await
            .context("abrindo o chaveiro padrão")?;
        if c.is_locked().await.unwrap_or(true) {
            return Err(KeyringLocked.into());
        }
        Ok(c)
    }

    async fn find(&self, c: &oo7::dbus::Collection, account: &str) -> Result<Option<TokenSet>> {
        let attrs = [("application", APP_ATTR), ("account", account)];
        let items = c
            .search_items(&attrs)
            .await
            .context("buscando no chaveiro")?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = item.secret().await.context("lendo o segredo")?;
        Ok(Some(
            serde_json::from_slice(secret.as_bytes()).context("segredo do chaveiro inválido")?,
        ))
    }

    async fn put(&self, c: &oo7::dbus::Collection, account: &str, t: &TokenSet) -> Result<()> {
        let attrs = [("application", APP_ATTR), ("account", account)];
        let json = serde_json::to_vec(t)?;
        c.create_item(
            &format!("omarchy-drive: {account}"),
            &attrs,
            json,
            true,
            None,
        )
        .await
        .context("gravando no chaveiro")?;
        Ok(())
    }

    /// Move o arquivo da conta para o keyring. O arquivo só é apagado depois
    /// de o item ser relido igual; qualquer falha mantém o arquivo.
    async fn migrate(&self, c: &oo7::dbus::Collection, account: &str, t: &TokenSet) -> Result<()> {
        self.put(c, account, t).await?;
        match self.find(c, account).await? {
            Some(back) if back.refresh_token == t.refresh_token => {
                self.legacy.remove(account)?;
                info!(account, "tokens migrados do arquivo para o chaveiro");
                Ok(())
            }
            _ => bail!("o item gravado no chaveiro não confere; o arquivo de token foi mantido"),
        }
    }
}

#[async_trait]
impl TokenStore for KeyringStore {
    async fn load(&self, account: &str) -> Result<TokenSet> {
        let c = self.collection().await?;
        if let Some(t) = self.find(&c, account).await? {
            // Sobra de uma migração interrompida: o keyring já vale.
            if self.legacy.path(account).exists() {
                let _ = self.legacy.remove(account);
            }
            return Ok(t);
        }
        let Some(t) = self.legacy.read(account)? else {
            return Err(not_authorized(account));
        };
        if let Err(e) = self.migrate(&c, account, &t).await {
            warn!(
                account,
                "migração para o chaveiro falhou, usando o arquivo: {e:#}"
            );
        }
        Ok(t)
    }

    async fn save(&self, account: &str, tokens: &TokenSet) -> Result<()> {
        let c = self.collection().await?;
        self.put(&c, account, tokens).await
    }

    async fn delete(&self, account: &str) -> Result<()> {
        let c = self.collection().await?;
        let attrs = [("application", APP_ATTR), ("account", account)];
        for item in c.search_items(&attrs).await? {
            item.delete(None).await.context("apagando do chaveiro")?;
        }
        self.legacy.remove(account)
    }

    fn kind(&self) -> &'static str {
        "keyring"
    }
}

// ---------------------------------------------------------------------------

/// Tokens só em memória: usado durante o login, antes de saber de qual conta
/// eles são (nada vai para o disco ou para o keyring nessa etapa).
pub struct MemoryStore(tokio::sync::Mutex<TokenSet>);

impl MemoryStore {
    pub fn new(t: TokenSet) -> Self {
        Self(tokio::sync::Mutex::new(t))
    }
    pub async fn get(&self) -> TokenSet {
        self.0.lock().await.clone()
    }
}

#[async_trait]
impl TokenStore for MemoryStore {
    async fn load(&self, _account: &str) -> Result<TokenSet> {
        Ok(self.0.lock().await.clone())
    }
    async fn save(&self, _account: &str, tokens: &TokenSet) -> Result<()> {
        *self.0.lock().await = tokens.clone();
        Ok(())
    }
    async fn delete(&self, _account: &str) -> Result<()> {
        Ok(())
    }
    fn kind(&self) -> &'static str {
        "memória"
    }
}

// ---------------------------------------------------------------------------

/// Escolhe o armazenamento: keyring se houver Secret Service na sessão, senão
/// arquivo (com aviso). Não desbloqueia nada: um keyring bloqueado é detectado
/// em cada operação.
pub async fn detect(tokens_dir: &Path) -> Arc<dyn TokenStore> {
    let legacy = FileStore::new(tokens_dir.to_path_buf());
    match tokio::time::timeout(Duration::from_secs(5), oo7::dbus::Service::new()).await {
        Ok(Ok(service)) => Arc::new(KeyringStore { service, legacy }),
        Ok(Err(e)) => {
            warn!("Secret Service indisponível ({e}); tokens ficam em arquivo 0600");
            Arc::new(legacy)
        }
        Err(_) => {
            warn!("Secret Service não respondeu; tokens ficam em arquivo 0600");
            Arc::new(legacy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(rt: &str) -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: rt.into(),
            expires_at: 1,
        }
    }

    #[tokio::test]
    async fn arquivo_com_permissoes_restritas() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path().join("tokens"));
        assert!(s.load("x").await.is_err());
        s.save("x", &tokens("r1")).await.unwrap();
        assert_eq!(s.load("x").await.unwrap().refresh_token, "r1");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&s.path("x")), 0o600);
        assert_eq!(mode(&dir.path().join("tokens")), 0o700);
        s.delete("x").await.unwrap();
        s.delete("x").await.unwrap();
        assert!(s.load("x").await.is_err());
    }

    /// Usa o keyring real da sessão (item de teste, apagado no fim). Rode com:
    /// `cargo test -p omarchy-drive-core keyring_real -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn keyring_real_grava_migra_e_apaga() {
        let dir = tempfile::tempdir().unwrap();
        let store = detect(dir.path()).await;
        assert_eq!(store.kind(), "keyring", "sem Secret Service nesta sessão");
        let acc = format!("teste-{}", std::process::id());
        // Migração: só existe o arquivo.
        FileStore::new(dir.path().to_path_buf())
            .write(&acc, &tokens("r-arquivo"))
            .unwrap();
        assert_eq!(store.load(&acc).await.unwrap().refresh_token, "r-arquivo");
        assert!(
            !dir.path().join(format!("{acc}.json")).exists(),
            "arquivo deveria sumir após a migração"
        );
        // Gravação e releitura.
        store.save(&acc, &tokens("r-novo")).await.unwrap();
        assert_eq!(store.load(&acc).await.unwrap().refresh_token, "r-novo");
        // Remoção.
        store.delete(&acc).await.unwrap();
        assert!(store.load(&acc).await.is_err());
    }
}
