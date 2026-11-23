//! Árvore remota em memória: id → item, com resolução de caminhos relativos
//! à pasta raiz do par.

use std::collections::HashMap;

use crate::ignore::Ignore;
use crate::model::{RelPath, RemoteEntry, RemoteItem, join};

#[derive(Default, Clone)]
pub struct RemoteTree {
    pub root_id: String,
    pub items: HashMap<String, RemoteItem>,
}

impl RemoteTree {
    pub fn new(root_id: String, items: Vec<RemoteItem>) -> Self {
        Self { root_id, items: items.into_iter().map(|i| (i.id.clone(), i)).collect() }
    }

    /// Caminho relativo do item, ou `None` se não estiver sob a raiz.
    pub fn path_of(&self, id: &str) -> Option<RelPath> {
        let mut parts = Vec::new();
        let mut cur = self.items.get(id)?;
        for _ in 0..512 {
            parts.push(cur.name.as_str());
            let parent = cur.parent_id.as_deref()?;
            if parent == self.root_id {
                parts.reverse();
                return Some(parts.join("/"));
            }
            cur = self.items.get(parent)?;
        }
        None
    }

    /// Todos os itens sob a raiz, indexados por caminho. Nomes duplicados na
    /// mesma pasta (possíveis no Google Drive) são desambiguados com o id e
    /// reportados em `dups`.
    pub fn entries(&self, ignore: &Ignore) -> (HashMap<RelPath, RemoteEntry>, Vec<RelPath>) {
        let mut children: HashMap<&str, Vec<&RemoteItem>> = HashMap::new();
        for i in self.items.values() {
            if let Some(p) = i.parent_id.as_deref() {
                children.entry(p).or_default().push(i);
            }
        }
        let mut out = HashMap::new();
        let mut dups = Vec::new();
        let mut stack: Vec<(String, RelPath)> = vec![(self.root_id.clone(), String::new())];
        while let Some((id, rel)) = stack.pop() {
            let Some(kids) = children.get(id.as_str()) else { continue };
            let mut seen: HashMap<&str, usize> = HashMap::new();
            for k in kids {
                *seen.entry(k.name.as_str()).or_default() += 1;
            }
            for k in kids {
                if k.name.is_empty() || k.name.contains('/') || k.name == "." || k.name == ".." {
                    continue;
                }
                let name = if seen[k.name.as_str()] > 1 {
                    // Mesmo nome para itens diferentes: o mais estável é
                    // sufixar o id; o usuário é avisado no status.
                    let p = join(&rel, &k.name);
                    if !dups.contains(&p) {
                        dups.push(p);
                    }
                    dedup_name(&k.name, &k.id)
                } else {
                    k.name.clone()
                };
                let path = join(&rel, &name);
                if ignore.is_ignored(&path) {
                    continue;
                }
                if k.is_dir {
                    stack.push((k.id.clone(), path.clone()));
                }
                out.insert(path.clone(), RemoteEntry { path, item: (*k).clone() });
            }
        }
        (out, dups)
    }
}

fn dedup_name(name: &str, id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({short}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({short})"),
    }
}
