//! Minimal SQLite store for the gars service.
//!
//! Scope (post v0.4): only the data that genuinely benefits from a relational
//! engine lives here. That means high-frequency append-and-poll patterns
//! (tasks, task_events) and the L4 search index. Everything else
//! (plans, subagents, connectors, schedules, service state) is reconstructed
//! from the filesystem and `~/.gars/state.json` by the server layer.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub status: String,
    pub input: String,
    pub result: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskEventRecord {
    pub id: i64,
    pub task_id: String,
    pub event_type: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct L4IndexEntry {
    pub id: String,
    pub path: String,
    pub summary: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct L4Hit {
    pub entry: L4IndexEntry,
    pub score: f64,
}

impl Store {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn init(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = self.conn()?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                input TEXT NOT NULL,
                result TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );

            CREATE TABLE IF NOT EXISTS l4_index (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                summary TEXT,
                created_at TEXT NOT NULL
            );
            "#,
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
            params![now()],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(2, ?1)",
            params![now()],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(3, ?1)",
            params![now()],
        )?;
        // v0.4 cleanup: remove legacy tables if present.
        for stmt in [
            "DROP TABLE IF EXISTS plans",
            "DROP TABLE IF EXISTS subagent_runs",
            "DROP TABLE IF EXISTS connector_state",
            "DROP TABLE IF EXISTS schedules",
            "DROP TABLE IF EXISTS service_state",
            "DROP TABLE IF EXISTS sessions",
            "DROP TABLE IF EXISTS messages",
        ] {
            conn.execute(stmt, [])?;
        }
        Ok(())
    }

    pub fn create_task(&self, input: &str) -> Result<TaskRecord> {
        let id = Uuid::new_v4().to_string();
        let ts = now();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO tasks(id, status, input, created_at, updated_at) VALUES(?1, 'queued', ?2, ?3, ?3)",
            params![id, input, ts],
        )?;
        self.append_event_with_conn(&conn, &id, "queued", serde_json::json!({"input": input}))?;
        self.get_task(&id)?
            .ok_or_else(|| anyhow::anyhow!("task vanished after insert"))
    }

    pub fn set_task_status(&self, id: &str, status: &str, result: Option<&str>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE tasks SET status = ?2, result = COALESCE(?3, result), updated_at = ?4 WHERE id = ?1",
            params![id, status, result, now()],
        )?;
        self.append_event_with_conn(
            &conn,
            id,
            status,
            serde_json::json!({"status": status, "result": result}),
        )
    }

    pub fn append_event(&self, task_id: &str, event_type: &str, payload: Value) -> Result<()> {
        let conn = self.conn()?;
        self.append_event_with_conn(&conn, task_id, event_type, payload)
    }

    pub fn list_tasks(&self, limit: usize) -> Result<Vec<TaskRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, status, input, result, created_at, updated_at FROM tasks ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(TaskRecord {
                id: row.get(0)?,
                status: row.get(1)?,
                input: row.get(2)?,
                result: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<TaskRecord>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, status, input, result, created_at, updated_at FROM tasks WHERE id = ?1",
            params![id],
            |row| {
                Ok(TaskRecord {
                    id: row.get(0)?,
                    status: row.get(1)?,
                    input: row.get(2)?,
                    result: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn task_events_after(&self, task_id: &str, after_id: i64) -> Result<Vec<TaskEventRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, event_type, payload, created_at FROM task_events WHERE task_id = ?1 AND id > ?2 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![task_id, after_id], |row| {
            let payload: String = row.get(3)?;
            Ok(TaskEventRecord {
                id: row.get(0)?,
                task_id: row.get(1)?,
                event_type: row.get(2)?,
                payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn l4_upsert(&self, entry: &L4IndexEntry) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO l4_index(id, path, summary, created_at) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET path=excluded.path, summary=excluded.summary",
            params![entry.id, entry.path, entry.summary, entry.created_at],
        )?;
        Ok(())
    }

    pub fn l4_get(&self, id: &str) -> Result<Option<L4IndexEntry>> {
        self.conn()?
            .query_row(
                "SELECT id, path, summary, created_at FROM l4_index WHERE id = ?1",
                params![id],
                |row| {
                    Ok(L4IndexEntry {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        summary: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn l4_search(&self, query: &str, k: usize) -> Result<Vec<L4Hit>> {
        let conn = self.conn()?;
        let pat = format!("%{}%", query.to_lowercase());
        let limit = if k == 0 { 100 } else { k as i64 };
        let mut stmt = conn.prepare(
            "SELECT id, path, summary, created_at FROM l4_index
             WHERE LOWER(summary) LIKE ?1 OR LOWER(path) LIKE ?1
             ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pat, limit], |row| {
            Ok(L4IndexEntry {
                id: row.get(0)?,
                path: row.get(1)?,
                summary: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            let entry = row?;
            let score = score_match(&entry, query);
            out.push(L4Hit { entry, score });
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(out)
    }

    fn append_event_with_conn(
        &self,
        conn: &Connection,
        task_id: &str,
        event_type: &str,
        payload: Value,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO task_events(task_id, event_type, payload, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![task_id, event_type, serde_json::to_string(&payload)?, now()],
        )?;
        Ok(())
    }

    fn conn(&self) -> Result<Connection> {
        Ok(Connection::open(&self.path)?)
    }
}

fn score_match(entry: &L4IndexEntry, query: &str) -> f64 {
    let q = query.to_lowercase();
    let summary = entry.summary.to_lowercase();
    let mut score = 0.0;
    for term in q.split_whitespace() {
        if term.is_empty() {
            continue;
        }
        if summary.contains(term) {
            score += 1.0;
        }
        if entry.path.to_lowercase().contains(term) {
            score += 0.5;
        }
    }
    score
}

fn now() -> String {
    let dt: DateTime<Local> = Utc::now().with_timezone(&Local);
    dt.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_and_tracks_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("gars.db"));
        store.init().unwrap();
        let task = store.create_task("hello").unwrap();
        store.set_task_status(&task.id, "done", Some("ok")).unwrap();
        let events = store.task_events_after(&task.id, 0).unwrap();
        assert!(events.len() >= 2);
        assert_eq!(store.get_task(&task.id).unwrap().unwrap().status, "done");
        let recent = store.list_tasks(5).unwrap();
        assert_eq!(recent[0].id, task.id);
    }

    #[test]
    fn l4_index_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("gars.db"));
        store.init().unwrap();
        let entry = L4IndexEntry {
            id: "a".into(),
            path: "/x/a.txt".into(),
            summary: "hello world".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        store.l4_upsert(&entry).unwrap();
        let hits = store.l4_search("hello", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn legacy_tables_are_dropped_on_reinit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gars.db");
        // Simulate a v0.3 database with the legacy tables.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE plans(id TEXT PRIMARY KEY);
             CREATE TABLE subagent_runs(run_id TEXT PRIMARY KEY);
             CREATE TABLE connector_state(id TEXT PRIMARY KEY);
             CREATE TABLE service_state(key TEXT PRIMARY KEY);
             CREATE TABLE schedules(id TEXT PRIMARY KEY);
             CREATE TABLE sessions(id TEXT PRIMARY KEY);
             CREATE TABLE messages(id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        drop(conn);
        let store = Store::new(&path);
        store.init().unwrap();
        let conn = Connection::open(&path).unwrap();
        for table in [
            "plans",
            "subagent_runs",
            "connector_state",
            "service_state",
            "schedules",
            "sessions",
            "messages",
        ] {
            let exists: Result<String, _> = conn.query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            );
            assert!(
                exists.is_err(),
                "expected table {table} to be dropped, but it exists"
            );
        }
    }
}
