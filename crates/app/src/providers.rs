//! Fábrica de provedores a partir da configuração, incluindo a escolha do
//! client OAuth de cada conta.

use std::sync::Arc;

use anyhow::{Result, bail};
use omarchy_drive_core::Provider;
use omarchy_drive_core::config::{Account, Config, ProviderKind};
use omarchy_drive_core::oauth::{OAuthEndpoints, TokenManager};
use omarchy_drive_core::token_store::TokenStore;

/// Client OAuth (id e, no Google, o secret de app instalado, que não é confidencial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub secret: Option<String>,
}

const README_HINT: &str = "veja a seção \"Credenciais OAuth\" do README";

/// Client padrão embutido na compilação (`packaging/oauth.env`), se houver.
fn default_client(kind: ProviderKind) -> Option<Client> {
    let (id, secret) = match kind {
        ProviderKind::Gdrive => (
            option_env!("OMARCHY_DRIVE_GOOGLE_CLIENT_ID"),
            option_env!("OMARCHY_DRIVE_GOOGLE_CLIENT_SECRET"),
        ),
        ProviderKind::Onedrive => (option_env!("OMARCHY_DRIVE_MS_CLIENT_ID"), None),
    };
    let id = id.filter(|s| !s.is_empty())?;
    Some(Client { id: id.into(), secret: secret.filter(|s| !s.is_empty()).map(str::to_owned) })
}

/// Client configurado pelo usuário com `omarchy-drive setup`.
fn configured_client(cfg: &Config, kind: ProviderKind) -> Option<Client> {
    match kind {
        ProviderKind::Gdrive => cfg
            .oauth
            .google_client_id
            .clone()
            .map(|id| Client { id, secret: cfg.oauth.google_client_secret.clone() }),
        ProviderKind::Onedrive => cfg.oauth.microsoft_client_id.clone().map(|id| Client { id, secret: None }),
    }
}

/// Client para uma conta nova: o configurado tem prioridade sobre o padrão.
fn pick_new(configured: Option<Client>, default: Option<Client>, kind: ProviderKind) -> Result<Client> {
    match configured.or(default) {
        Some(c) => Ok(c),
        None => bail!(
            "nenhum client OAuth disponível para {kind}: configure um com `omarchy-drive setup {}` ({README_HINT})",
            if kind == ProviderKind::Gdrive { "google" } else { "microsoft" }
        ),
    }
}

/// Client de uma conta existente: sempre o que a autorizou.
fn pick_for_account(
    account_client: Option<&str>,
    configured: Option<Client>,
    default: Option<Client>,
    kind: ProviderKind,
) -> Result<Client> {
    let Some(id) = account_client else {
        // Conta criada antes do registro do client: usa `[oauth]`, como sempre usou.
        return pick_new(configured, None, kind);
    };
    for c in [configured, default].into_iter().flatten() {
        if c.id == id {
            return Ok(c);
        }
    }
    bail!("o client OAuth que autorizou esta conta ({id}) não está mais disponível; reconecte a conta ({README_HINT})")
}

pub fn client_for_new(cfg: &Config, kind: ProviderKind) -> Result<Client> {
    pick_new(configured_client(cfg, kind), default_client(kind), kind)
}

pub fn client_for_account(cfg: &Config, account: &Account) -> Result<Client> {
    pick_for_account(
        account.client_id.as_deref(),
        configured_client(cfg, account.provider),
        default_client(account.provider),
        account.provider,
    )
}

pub fn endpoints(kind: ProviderKind, client: &Client) -> OAuthEndpoints {
    match kind {
        ProviderKind::Gdrive => omarchy_drive_gdrive::endpoints(client.id.clone(), client.secret.clone()),
        ProviderKind::Onedrive => omarchy_drive_onedrive::endpoints(client.id.clone()),
    }
}

pub fn build(cfg: &Config, store: &Arc<dyn TokenStore>, account: &Account) -> Result<Arc<dyn Provider>> {
    let ep = endpoints(account.provider, &client_for_account(cfg, account)?);
    let tokens = TokenManager::new(ep, store.clone(), account.name.clone());
    Ok(match account.provider {
        ProviderKind::Gdrive => Arc::new(omarchy_drive_gdrive::GDrive::new(tokens)),
        ProviderKind::Onedrive => Arc::new(omarchy_drive_onedrive::OneDrive::new(tokens)),
    })
}

/// E-mail da conta dona dos tokens (ainda só em memória, durante o login).
pub async fn email_from_tokens(kind: ProviderKind, ep: OAuthEndpoints, store: Arc<dyn TokenStore>) -> Result<String> {
    let tokens = TokenManager::new(ep, store, "pendente");
    match kind {
        ProviderKind::Gdrive => omarchy_drive_gdrive::GDrive::new(tokens).account_email().await,
        ProviderKind::Onedrive => omarchy_drive_onedrive::OneDrive::new(tokens).account_email().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str) -> Option<Client> {
        Some(Client { id: id.into(), secret: Some(format!("s-{id}")) })
    }
    const G: ProviderKind = ProviderKind::Gdrive;

    #[test]
    fn conta_nova_prefere_o_client_configurado() {
        assert_eq!(pick_new(c("meu"), c("padrao"), G).unwrap().id, "meu");
        assert_eq!(pick_new(None, c("padrao"), G).unwrap().id, "padrao");
        let e = pick_new(None, None, G).unwrap_err().to_string();
        assert!(e.contains("setup google") && e.contains("README"), "{e}");
    }

    #[test]
    fn conta_existente_usa_o_client_que_a_autorizou() {
        // Autorizada pelo padrão; o usuário configurou outro client depois.
        let got = pick_for_account(Some("padrao"), c("meu"), c("padrao"), G).unwrap();
        assert_eq!(got, c("padrao").unwrap());
        // Autorizada pelo client próprio.
        assert_eq!(pick_for_account(Some("meu"), c("meu"), c("padrao"), G).unwrap().id, "meu");
    }

    #[test]
    fn conta_antiga_sem_registro_usa_oauth_configurado() {
        assert_eq!(pick_for_account(None, c("meu"), c("padrao"), G).unwrap().id, "meu");
        // Sem `[oauth]`, conta antiga não cai no padrão (foi autorizada por outro client).
        assert!(pick_for_account(None, None, c("padrao"), G).is_err());
    }

    #[test]
    fn client_sumiu() {
        let e = pick_for_account(Some("antigo"), c("meu"), c("padrao"), G).unwrap_err().to_string();
        assert!(e.contains("não está mais disponível"), "{e}");
    }
}
