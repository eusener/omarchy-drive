//! Fábrica de provedores a partir da configuração.

use std::sync::Arc;

use anyhow::{Context, Result};
use omarchy_drive_core::Provider;
use omarchy_drive_core::config::{Account, Config, Dirs, ProviderKind};
use omarchy_drive_core::oauth::{OAuthEndpoints, TokenManager};

pub fn endpoints(cfg: &Config, kind: ProviderKind) -> Result<OAuthEndpoints> {
    Ok(match kind {
        ProviderKind::Gdrive => omarchy_drive_gdrive::endpoints(
            cfg.oauth.google_client_id.clone().context(
                "client OAuth do Google não configurado. Rode `omarchy-drive setup google --client-id ... --client-secret ...`",
            )?,
            cfg.oauth.google_client_secret.clone(),
        ),
        ProviderKind::Onedrive => omarchy_drive_onedrive::endpoints(
            cfg.oauth.microsoft_client_id.clone().context(
                "app da Microsoft não configurado. Rode `omarchy-drive setup microsoft --client-id ...`",
            )?,
        ),
    })
}

pub fn build(cfg: &Config, dirs: &Dirs, account: &Account) -> Result<Arc<dyn Provider>> {
    let tokens = TokenManager::load(endpoints(cfg, account.provider)?, dirs.token_file(&account.name))?;
    Ok(match account.provider {
        ProviderKind::Gdrive => Arc::new(omarchy_drive_gdrive::GDrive::new(tokens)),
        ProviderKind::Onedrive => Arc::new(omarchy_drive_onedrive::OneDrive::new(tokens)),
    })
}

/// E-mail da conta recém-autorizada (para exibição).
pub async fn account_email(cfg: &Config, dirs: &Dirs, account: &Account) -> Result<String> {
    let tokens = TokenManager::load(endpoints(cfg, account.provider)?, dirs.token_file(&account.name))?;
    match account.provider {
        ProviderKind::Gdrive => omarchy_drive_gdrive::GDrive::new(tokens).account_email().await,
        ProviderKind::Onedrive => omarchy_drive_onedrive::OneDrive::new(tokens).account_email().await,
    }
}
