//! Regras de ignorar arquivos (temporários de editores, lixo de SO etc.).

use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Padrões embutidos: arquivos temporários que nunca devem subir.
pub const BUILTIN: &[&str] = &[
    ".omarchy-drive*",
    "*.omarchy-drive-tmp",
    ".~lock.*#",
    "~$*",
    "*~",
    "*.swp",
    "*.swx",
    ".*.swp",
    "*.tmp",
    "*.part",
    "*.crdownload",
    ".goutputstream-*",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    ".Trash-*",
];

#[derive(Clone)]
pub struct Ignore {
    names: GlobSet,
    paths: GlobSet,
    include: Vec<String>,
}

impl Ignore {
    /// `extra`: padrões do usuário. Padrões com `/` casam o caminho relativo
    /// inteiro; sem `/`, casam o nome em qualquer nível.
    /// `include`: sync seletivo (prefixos de caminho).
    pub fn new(extra: &[String], include: &[String]) -> Result<Self> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        for p in BUILTIN
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str))
        {
            if p.contains('/') {
                paths.add(Glob::new(p.trim_start_matches('/'))?);
            } else {
                names.add(Glob::new(p)?);
            }
        }
        Ok(Self {
            names: names.build()?,
            paths: paths.build()?,
            include: include
                .iter()
                .map(|s| s.trim_matches('/').to_string())
                .collect(),
        })
    }

    pub fn is_ignored(&self, rel: &str) -> bool {
        if rel.split('/').any(|seg| self.names.is_match(seg)) || self.paths.is_match(rel) {
            return true;
        }
        if self.include.is_empty() {
            return false;
        }
        // Mantém o caminho se estiver dentro de um include ou for ancestral
        // de um (para as pastas intermediárias existirem).
        !self.include.iter().any(|inc| {
            rel == inc || crate::model::is_under(rel, inc) || crate::model::is_under(inc, rel)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_and_selective() {
        let ig = Ignore::new(&["build/**".into()], &["Docs".into()]).unwrap();
        assert!(ig.is_ignored("Docs/.~lock.a.odt#"));
        assert!(ig.is_ignored("Docs/x.tmp"));
        assert!(!ig.is_ignored("Docs/a.odt"));
        assert!(!ig.is_ignored("Docs"));
        assert!(ig.is_ignored("Fotos/a.jpg"));
        let ig = Ignore::new(&["build/**".into()], &[]).unwrap();
        assert!(ig.is_ignored("build/out.o"));
        assert!(!ig.is_ignored("src/build.rs"));
    }
}
