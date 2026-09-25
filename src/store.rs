use crate::model::{Event, Workload};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::path::Path;

pub struct Record {
    pub workload: Workload,
    pub desired: bool,
    pub next_run: Option<DateTime<Utc>>,
}
pub struct Store {
    db: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(
            version <= 1,
            "Database was created by a newer K3 Up version; refusing to downgrade it"
        );
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL with NORMAL syncs once per checkpoint instead of per commit. A power cut can lose
        // the last few events and state changes, such as a stop request, but never corrupts
        // the database.
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
            CREATE TABLE IF NOT EXISTS workloads (name TEXT PRIMARY KEY, spec TEXT NOT NULL, desired INTEGER NOT NULL, next_run TEXT);
            CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, at TEXT NOT NULL, name TEXT NOT NULL, message TEXT NOT NULL);
            PRAGMA user_version=1;")?;
        Ok(Self { db })
    }
    pub fn load(&self) -> Result<Vec<Record>> {
        let mut statement = self
            .db
            .prepare("SELECT spec, desired, next_run FROM workloads ORDER BY name")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (spec, desired, next_run) = row?;
            Ok(Record {
                workload: serde_json::from_str(&spec)?,
                desired,
                next_run: next_run.map(|s| s.parse()).transpose()?,
            })
        })
        .collect()
    }
    pub fn save_batch(&mut self, records: &[Record]) -> Result<()> {
        let tx = self.db.transaction()?;
        for record in records {
            tx.execute("INSERT INTO workloads(name,spec,desired,next_run) VALUES (?1,?2,?3,?4)
                ON CONFLICT(name) DO UPDATE SET spec=excluded.spec,desired=excluded.desired,next_run=excluded.next_run",
                params![record.workload.name, serde_json::to_string(&record.workload)?, record.desired, record.next_run.map(|d| d.to_rfc3339())])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn runtime(&self, name: &str, desired: bool, next: Option<DateTime<Utc>>) -> Result<()> {
        self.db
            .prepare_cached("UPDATE workloads SET desired=?2,next_run=?3 WHERE name=?1")?
            .execute(params![name, desired, next.map(|d| d.to_rfc3339())])?;
        Ok(())
    }
    pub fn remove(&self, name: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM workloads WHERE name=?1", [name])?;
        Ok(())
    }
    pub fn event(&self, name: &str, message: &str) -> Result<()> {
        self.db
            .prepare_cached("INSERT INTO events(at,name,message) VALUES (?1,?2,?3)")?
            .execute(params![Utc::now().to_rfc3339(), name, message])?;
        // Keeps the newest 2,000 to 2,500 events; pruning in batches avoids a delete per insert.
        let id = self.db.last_insert_rowid();
        if id % 500 == 0 {
            self.db
                .prepare_cached("DELETE FROM events WHERE id <= ?1")?
                .execute([id - 2000])?;
        }
        Ok(())
    }
    pub fn events(&self, name: Option<&str>, after: Option<i64>) -> Result<Vec<Event>> {
        let mut statement = self.db.prepare_cached("SELECT id,at,name,message FROM events WHERE (?1 IS NULL OR name=?1) AND id > ?2 ORDER BY id DESC LIMIT 200")?;
        let rows = statement.query_map(params![name, after.unwrap_or(0)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (id, at, name, message) = row?;
            Ok(Event {
                id,
                at: at.parse()?,
                name,
                message,
            })
        })
        .collect()
    }
}
