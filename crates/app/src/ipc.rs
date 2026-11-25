//! Socket de controle do daemon: uma requisição JSON por linha.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    SyncNow { pair: Option<String> },
    Pause { pair: Option<String> },
    Resume { pair: Option<String> },
    ConfirmDeletes { pair: String },
    ClearConflicts { pair: Option<String> },
    Reload,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
}

pub async fn call(socket: &Path, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket)
        .await
        .context("daemon não está rodando (inicie com `systemctl --user start omarchy-drive`)")?;
    let (r, mut w) = stream.into_split();
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    let mut lines = BufReader::new(r).lines();
    let resp = lines
        .next_line()
        .await?
        .context("daemon fechou a conexão")?;
    Ok(serde_json::from_str(&resp)?)
}
