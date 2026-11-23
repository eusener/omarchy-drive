//! Status compartilhado com a CLI e o plugin da barra via
//! `$XDG_RUNTIME_DIR/omarchy-drive/status.json` (escrita atômica).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PairState {
    #[default]
    Idle,
    Syncing,
    Paused,
    Offline,
    Error,
    /// Exclusão em massa retida aguardando `omarchy-drive confirm <par>`.
    NeedsConfirm,
    /// O provedor recusou o login da conta; só reautorizando volta a sincronizar.
    AuthExpired,
}

/// Estado de autenticação de uma conta, exibido no painel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    #[default]
    Ok,
    Expired,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountStatus {
    pub provider: String,
    /// E-mail da conta.
    pub label: String,
    pub auth: AuthState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairStatus {
    pub account: String,
    pub provider: String,
    pub local: String,
    pub remote: String,
    pub state: PairState,
    pub last_sync: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// Ações restantes no ciclo atual.
    pub queued: usize,
    /// Arquivos em transferência agora.
    pub active: Vec<String>,
    pub uploaded: u64,
    pub downloaded: u64,
    pub pending_deletes: usize,
    pub conflicts: Vec<String>,
    pub warnings: Vec<String>,
    pub quota_used: Option<u64>,
    pub quota_total: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusDoc {
    pub version: u32,
    pub pid: u32,
    pub updated_at: Option<DateTime<Utc>>,
    /// Resumo para a barra: idle | syncing | paused | offline | error | needs_confirm.
    pub overall: PairState,
    pub pairs: BTreeMap<String, PairStatus>,
    /// Contas configuradas (versão 2 em diante).
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountStatus>,
}

/// Versão atual do formato de `status.json`.
pub const STATUS_VERSION: u32 = 2;

impl StatusDoc {
    fn recompute(&mut self) {
        use PairState::*;
        let states: Vec<PairState> = self.pairs.values().map(|p| p.state).collect();
        self.overall = [NeedsConfirm, AuthExpired, Error, Syncing, Offline]
            .into_iter()
            .find(|s| states.contains(s))
            .unwrap_or(if !states.is_empty() && states.iter().all(|s| *s == Paused) {
                Paused
            } else {
                Idle
            });
    }
}

pub struct StatusHub {
    path: Option<PathBuf>,
    doc: Mutex<StatusDoc>,
}

impl StatusHub {
    pub fn new(path: Option<PathBuf>) -> Self {
        let doc = StatusDoc { version: STATUS_VERSION, pid: std::process::id(), ..Default::default() };
        Self { path, doc: Mutex::new(doc) }
    }

    pub fn update(&self, pair: &str, f: impl FnOnce(&mut PairStatus)) {
        let snapshot = {
            let mut d = self.doc.lock().unwrap();
            f(d.pairs.entry(pair.to_string()).or_default());
            d.updated_at = Some(Utc::now());
            d.recompute();
            d.clone()
        };
        self.write(&snapshot);
    }

    pub fn remove(&self, pair: &str) {
        let snapshot = {
            let mut d = self.doc.lock().unwrap();
            d.pairs.remove(pair);
            d.recompute();
            d.clone()
        };
        self.write(&snapshot);
    }

    /// Substitui a lista de contas (na partida e a cada recarga da configuração),
    /// preservando o estado de autenticação já conhecido de cada conta.
    pub fn set_accounts(&self, accounts: impl IntoIterator<Item = (String, String, String)>) {
        let snapshot = {
            let mut d = self.doc.lock().unwrap();
            let old = std::mem::take(&mut d.accounts);
            for (name, provider, label) in accounts {
                let auth = old.get(&name).map(|a| a.auth).unwrap_or_default();
                d.accounts.insert(name, AccountStatus { provider, label, auth });
            }
            d.updated_at = Some(Utc::now());
            d.clone()
        };
        self.write(&snapshot);
    }

    /// Marca o estado de autenticação de uma conta. Devolve o estado anterior.
    pub fn set_account_auth(&self, account: &str, auth: AuthState) -> Option<AuthState> {
        let (prev, snapshot) = {
            let mut d = self.doc.lock().unwrap();
            let a = d.accounts.get_mut(account)?;
            let prev = a.auth;
            if prev == auth {
                return Some(prev);
            }
            a.auth = auth;
            d.updated_at = Some(Utc::now());
            (prev, d.clone())
        };
        self.write(&snapshot);
        Some(prev)
    }

    /// Grava o estado atual (ex.: na partida do daemon, mesmo sem pares).
    pub fn flush(&self) {
        let snapshot = {
            let mut d = self.doc.lock().unwrap();
            d.updated_at = Some(Utc::now());
            d.recompute();
            d.clone()
        };
        self.write(&snapshot);
    }

    pub fn snapshot(&self) -> StatusDoc {
        self.doc.lock().unwrap().clone()
    }

    fn write(&self, doc: &StatusDoc) {
        let Some(path) = &self.path else { return };
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = path.with_extension("json.tmp");
        if let Ok(s) = serde_json::to_vec_pretty(doc)
            && std::fs::write(&tmp, s).is_ok()
        {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_v2_com_contas_e_login_expirado() {
        let hub = StatusHub::new(None);
        hub.set_accounts([("pessoal".into(), "gdrive".into(), "a@b.c".into())]);
        hub.update("drive", |s| {
            s.account = "pessoal".into();
            s.state = PairState::AuthExpired;
        });
        hub.update("outro", |s| s.state = PairState::Error);
        assert_eq!(hub.set_account_auth("pessoal", AuthState::Expired), Some(AuthState::Ok));
        assert_eq!(hub.set_account_auth("pessoal", AuthState::Expired), Some(AuthState::Expired));
        assert_eq!(hub.set_account_auth("naoexiste", AuthState::Expired), None);

        let v = serde_json::to_value(hub.snapshot()).unwrap();
        assert_eq!(v["version"], 2);
        // Login expirado tem prioridade sobre erro genérico no resumo da barra.
        assert_eq!(v["overall"], "auth_expired");
        assert_eq!(v["pairs"]["drive"]["state"], "auth_expired");
        assert_eq!(v["accounts"]["pessoal"]["auth"], "expired");
        assert_eq!(v["accounts"]["pessoal"]["label"], "a@b.c");

        // Recarga da configuração preserva o estado de autenticação conhecido.
        hub.set_accounts([("pessoal".into(), "gdrive".into(), "a@b.c".into())]);
        assert_eq!(hub.snapshot().accounts["pessoal"].auth, AuthState::Expired);
    }

    #[test]
    fn status_v1_continua_legivel() {
        let v1 = r#"{"version":1,"pid":1,"updated_at":null,"overall":"idle","pairs":{}}"#;
        let d: StatusDoc = serde_json::from_str(v1).unwrap();
        assert!(d.accounts.is_empty());
    }
}
