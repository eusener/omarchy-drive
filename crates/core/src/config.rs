//! Configuração em `~/.config/omarchy-drive/config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Gdrive,
    Onedrive,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProviderKind::Gdrive => "gdrive",
            ProviderKind::Onedrive => "onedrive",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// Nome curto escolhido pelo usuário (ex.: `pessoal`, `trabalho`).
    pub name: String,
    pub provider: ProviderKind,
    /// E-mail/identificação mostrada no status.
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPair {
    pub name: String,
    pub account: String,
    /// Pasta local espelhada.
    pub local: PathBuf,
    /// Caminho da pasta no drive (`""` = raiz).
    #[serde(default)]
    pub remote: String,
    /// Sync seletivo: se não vazio, só estes subcaminhos são sincronizados.
    #[serde(default)]
    pub include: Vec<String>,
    /// Padrões glob extras a ignorar (além dos padrões embutidos).
    #[serde(default)]
    pub ignore: Vec<String>,
    /// Só baixa (nunca envia nem apaga no remoto).
    #[serde(default)]
    pub download_only: bool,
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Intervalo de consulta de mudanças remotas, em segundos.
    pub poll_interval_secs: u64,
    /// Proteção contra exclusão em massa: acima disso, o par é pausado e
    /// pede confirmação.
    pub max_deletes_without_confirm: usize,
    /// Transferências simultâneas.
    pub parallel_transfers: usize,
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_interval_secs: 30,
            max_deletes_without_confirm: 50,
            parallel_transfers: 4,
            notifications: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthClients {
    /// Client OAuth "Desktop app" do Google Cloud.
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    /// App "public client" registrado no Microsoft Entra.
    pub microsoft_client_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub oauth: OAuthClients,
    #[serde(default, rename = "account")]
    pub accounts: Vec<Account>,
    #[serde(default, rename = "pair")]
    pub pairs: Vec<SyncPair>,
}

pub struct Dirs {
    pub config: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
}

impl Dirs {
    pub fn get() -> Result<Self> {
        let base = directories::BaseDirs::new().context("diretório home não encontrado")?;
        let config = base.config_dir().join("omarchy-drive");
        let state = base
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| base.home_dir().join(".local/state"))
            .join("omarchy-drive");
        let runtime = base
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir)
            .join("omarchy-drive");
        Ok(Self { config, state, runtime })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
    pub fn token_file(&self, account: &str) -> PathBuf {
        self.state.join("tokens").join(format!("{account}.json"))
    }
    pub fn pair_db(&self, pair: &str) -> PathBuf {
        self.state.join("pairs").join(format!("{pair}.sqlite"))
    }
    pub fn status_file(&self) -> PathBuf {
        self.runtime.join("status.json")
    }
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("control.sock")
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("lendo {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&s).with_context(|| format!("config inválida: {}", path.display()))?;
        for p in &mut cfg.pairs {
            p.local = expand_tilde(&p.local);
            p.remote = p.remote.trim_matches('/').to_string();
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }
    pub fn pair(&self, name: &str) -> Option<&SyncPair> {
        self.pairs.iter().find(|p| p.name == name)
    }
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~")
        && let Some(base) = directories::BaseDirs::new()
    {
        return base.home_dir().join(rest);
    }
    p.to_path_buf()
}
