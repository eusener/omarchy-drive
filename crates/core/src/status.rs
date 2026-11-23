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
}

impl StatusDoc {
    fn recompute(&mut self) {
        use PairState::*;
        let states: Vec<PairState> = self.pairs.values().map(|p| p.state).collect();
        self.overall = [NeedsConfirm, Error, Syncing, Offline]
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
        let doc = StatusDoc { version: 1, pid: std::process::id(), ..Default::default() };
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
