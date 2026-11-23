//! Lixeira local no padrão freedesktop (`~/.local/share/Trash`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn default_trash_dir() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("sem home")?;
    Ok(base.data_dir().join("Trash"))
}

fn encode_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace("%2F", "/")
        .replace('+', "%20")
}

/// Move `path` para a lixeira `dir`, criando o `.trashinfo` correspondente.
pub fn trash(dir: &Path, path: &Path) -> Result<()> {
    let files = dir.join("files");
    let info = dir.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    let name = path.file_name().context("caminho sem nome")?.to_string_lossy().to_string();
    let mut candidate = name.clone();
    let mut n = 1;
    while files.join(&candidate).exists() || info.join(format!("{candidate}.trashinfo")).exists() {
        n += 1;
        candidate = format!("{name}.{n}");
    }
    let abs = std::path::absolute(path)?;
    let when = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    std::fs::write(
        info.join(format!("{candidate}.trashinfo")),
        format!("[Trash Info]\nPath={}\nDeletionDate={when}\n", encode_path(&abs)),
    )?;
    if let Err(e) = std::fs::rename(&abs, files.join(&candidate)) {
        let _ = std::fs::remove_file(info.join(format!("{candidate}.trashinfo")));
        return Err(e).with_context(|| format!("movendo {} para a lixeira", abs.display()));
    }
    Ok(())
}
