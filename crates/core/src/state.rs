//! Estado persistente de um par de sincronização (SQLite).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{BaseEntry, RelPath, RemoteItem};

pub struct State {
    conn: Connection,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS base (
    path TEXT PRIMARY KEY,
    is_dir INTEGER NOT NULL,
    remote_id TEXT NOT NULL,
    remote_rev TEXT NOT NULL,
    hash TEXT,
    size INTEGER NOT NULL,
    local_mtime_ns INTEGER NOT NULL,
    inode INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS remote (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    is_dir INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ms INTEGER NOT NULL,
    rev TEXT NOT NULL,
    hash TEXT,
    native INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS remote_parent ON remote(parent_id);
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

impl State {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    // ---- base ----

    pub fn load_base(&self) -> Result<HashMap<RelPath, BaseEntry>> {
        let mut st = self.conn.prepare(
            "SELECT path,is_dir,remote_id,remote_rev,hash,size,local_mtime_ns,inode FROM base",
        )?;
        let rows = st.query_map([], |r| {
            Ok(BaseEntry {
                path: r.get(0)?,
                is_dir: r.get(1)?,
                remote_id: r.get(2)?,
                remote_rev: r.get(3)?,
                hash: r.get(4)?,
                size: r.get::<_, i64>(5)? as u64,
                local_mtime_ns: r.get(6)?,
                inode: r.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut m = HashMap::new();
        for b in rows {
            let b = b?;
            m.insert(b.path.clone(), b);
        }
        Ok(m)
    }

    pub fn put_base(&self, b: &BaseEntry) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO base VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                b.path,
                b.is_dir,
                b.remote_id,
                b.remote_rev,
                b.hash,
                b.size as i64,
                b.local_mtime_ns,
                b.inode as i64
            ],
        )?;
        Ok(())
    }

    /// Remove `path` e tudo abaixo dele.
    pub fn forget_base(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM base WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            params![path, format!("{}/%", escape_like(path))],
        )?;
        Ok(())
    }

    /// Renomeia `from` (e a subárvore) para `to`.
    pub fn relocate_base(&mut self, from: &str, to: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM base WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            params![to, format!("{}/%", escape_like(to))],
        )?;
        tx.execute(
            "UPDATE base SET path = ?2 || substr(path, length(?1) + 1)
             WHERE path = ?1 OR path LIKE ?3 ESCAPE '\\'",
            params![from, to, format!("{}/%", escape_like(from))],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---- árvore remota ----

    pub fn load_remote(&self) -> Result<Vec<RemoteItem>> {
        let mut st = self.conn.prepare(
            "SELECT id,parent_id,name,is_dir,size,mtime_ms,rev,hash,native FROM remote",
        )?;
        let rows = st.query_map([], |r| {
            Ok(RemoteItem {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                name: r.get(2)?,
                is_dir: r.get(3)?,
                size: r.get::<_, i64>(4)? as u64,
                mtime_ms: r.get(5)?,
                rev: r.get(6)?,
                hash: r.get(7)?,
                native: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn upsert_remote(&self, items: &[RemoteItem]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st =
                tx.prepare("INSERT OR REPLACE INTO remote VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)")?;
            for i in items {
                st.execute(params![
                    i.id,
                    i.parent_id,
                    i.name,
                    i.is_dir,
                    i.size as i64,
                    i.mtime_ms,
                    i.rev,
                    i.hash,
                    i.native
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_remote(&self, ids: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut st = tx.prepare("DELETE FROM remote WHERE id = ?1")?;
            for id in ids {
                st.execute(params![id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn replace_remote(&self, items: &[RemoteItem]) -> Result<()> {
        self.conn.execute("DELETE FROM remote", [])?;
        self.upsert_remote(items)
    }

    // ---- kv ----

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute("INSERT OR REPLACE INTO kv VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    pub fn del(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(path: &str) -> BaseEntry {
        BaseEntry {
            path: path.into(),
            is_dir: false,
            remote_id: path.into(),
            remote_rev: "r".into(),
            hash: None,
            size: 0,
            local_mtime_ns: 0,
            inode: 0,
        }
    }

    #[test]
    fn relocate_and_forget_subtree() {
        let mut s = State::open_in_memory().unwrap();
        for p in ["a", "a/x", "a/y/z", "ab", "a_b/q"] {
            s.put_base(&b(p)).unwrap();
        }
        s.relocate_base("a", "c").unwrap();
        let mut keys: Vec<_> = s.load_base().unwrap().into_keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["a_b/q", "ab", "c", "c/x", "c/y/z"]);
        s.forget_base("c").unwrap();
        let mut keys: Vec<_> = s.load_base().unwrap().into_keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["a_b/q", "ab"]);
    }
}
