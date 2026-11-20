//! SQLite storage: one row per continuous block of work on a project.
//!
//! Every block has a `uuid` and the `machine` that counted it, so blocks from several
//! machines can live in one database (see `sync`). Deleting only marks a block, so the
//! deletion can reach the other machines; `updated_at` decides which change wins.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// One continuous block of work, as stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Block {
    pub id: i64,
    pub uuid: String,
    pub machine: String,
    pub project: String,
    pub editor: String,
    pub start: i64,
    pub end: i64,
}

/// Where blocks of time are written; the tracker only needs these two calls.
pub trait Sink {
    /// Starts a block and returns its id.
    fn open_block(&mut self, project: &str, editor: &str, start: i64) -> Result<i64>;
    /// Moves the end of a block.
    fn set_end(&mut self, id: i64, end: i64) -> Result<()>;
}

/// A change to a block, as written to and read from the sync files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Event {
    Put {
        id: String,
        machine: String,
        project: String,
        editor: String,
        start: i64,
        end: i64,
        at: i64,
    },
    Delete {
        id: String,
        at: i64,
    },
}

impl Event {
    pub fn id(&self) -> &str {
        match self {
            Event::Put { id, .. } | Event::Delete { id, .. } => id,
        }
    }
    pub fn at(&self) -> i64 {
        match self {
            Event::Put { at, .. } | Event::Delete { at, .. } => *at,
        }
    }
}

pub struct Store {
    conn: Connection,
    machine: String,
}

/// Current time, the clock used for `updated_at`.
fn clock() -> i64 {
    chrono::Local::now().timestamp()
}

const BLOCK_COLS: &str = "id, uuid, machine, project, editor, start, end";

fn block_row(r: &rusqlite::Row) -> rusqlite::Result<Block> {
    Ok(Block {
        id: r.get(0)?,
        uuid: r.get(1)?,
        machine: r.get(2)?,
        project: r.get(3)?,
        editor: r.get(4)?,
        start: r.get(5)?,
        end: r.get(6)?,
    })
}

impl Store {
    /// `$CODECLOCK_DB` when set (handy for trying things on a copy), else the data folder.
    pub fn default_path() -> PathBuf {
        if let Some(p) = std::env::var_os("CODECLOCK_DB") {
            return PathBuf::from(p);
        }
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("codeclock")
            .join("codeclock.db")
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let machine = crate::sync::default_machine();
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS blocks (
                 id      INTEGER PRIMARY KEY,
                 project TEXT    NOT NULL,
                 source  TEXT    NOT NULL DEFAULT 'editor',
                 editor  TEXT    NOT NULL DEFAULT '',
                 start   INTEGER NOT NULL,
                 end     INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS blocks_start ON blocks(start);
             CREATE INDEX IF NOT EXISTS blocks_project ON blocks(project);",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            // Version 1: identity, machine and soft deletes for syncing between machines.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE blocks ADD COLUMN uuid TEXT;
                 ALTER TABLE blocks ADD COLUMN machine TEXT NOT NULL DEFAULT '';
                 ALTER TABLE blocks ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE blocks ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE blocks ADD COLUMN synced INTEGER NOT NULL DEFAULT 0;
                 UPDATE blocks SET uuid = lower(hex(randomblob(16))), updated_at = end;
                 CREATE UNIQUE INDEX blocks_uuid ON blocks(uuid);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE sync_files (path TEXT PRIMARY KEY, offset INTEGER NOT NULL);
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
            conn.execute("UPDATE blocks SET machine = ?1 WHERE machine = ''", params![machine])?;
        }
        if version < 2 {
            // Version 2: no `source`; all time is yours, whoever did the typing.
            conn.execute_batch("ALTER TABLE blocks DROP COLUMN source; PRAGMA user_version = 2;")?;
        }
        Ok(Store { conn, machine })
    }

    /// The machine new blocks are recorded for.
    pub fn set_machine(&mut self, machine: &str) {
        self.machine = machine.to_string();
    }

    /// Seconds per project inside [from, to), blocks clipped to the window, largest first.
    pub fn totals(&self, from: i64, to: i64) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, SUM(MIN(end, ?2) - MAX(start, ?1)) AS secs
               FROM blocks
              WHERE end > ?1 AND start < ?2 AND deleted = 0
              GROUP BY project
             HAVING secs > 0
              ORDER BY secs DESC, project",
        )?;
        let rows = stmt.query_map(params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// All-time seconds for one project.
    pub fn project_total(&self, project: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(end - start), 0) FROM blocks WHERE project = ?1 AND deleted = 0",
            params![project],
            |r| r.get(0),
        )?)
    }

    /// Blocks that overlap [from, to), oldest first, not clipped.
    pub fn blocks(&self, from: i64, to: i64) -> Result<Vec<Block>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BLOCK_COLS} FROM blocks
              WHERE end > ?1 AND start < ?2 AND end > start AND deleted = 0
              ORDER BY start"
        ))?;
        let rows = stmt.query_map(params![from, to], block_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every project with its all-time seconds, largest first.
    pub fn projects(&self) -> Result<Vec<(String, i64)>> {
        self.totals(i64::MIN / 2, i64::MAX / 2)
    }

    /// Start of the oldest block.
    pub fn first_start(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT MIN(start) FROM blocks WHERE deleted = 0", [], |r| r.get(0)).optional()?.flatten())
    }

    /// Whether [start, end) overlaps another block of the same project as `id`
    /// (blocks of different projects can run side by side).
    pub fn overlaps(&self, id: i64, start: i64, end: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM blocks WHERE id != ?1 AND end > ?2 AND start < ?3 AND deleted = 0
                              AND project = (SELECT project FROM blocks WHERE id = ?1))",
            params![id, start, end],
            |r| r.get(0),
        )?)
    }

    /// Changes a block's project and times; false when there is no such block.
    pub fn update_block(&self, id: i64, project: &str, start: i64, end: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE blocks SET project = ?2, start = ?3, end = ?4, updated_at = ?5, synced = 0
              WHERE id = ?1 AND deleted = 0",
            params![id, project, start, end, clock()],
        )?;
        Ok(n > 0)
    }

    /// Removes a block (marks it, so the deletion syncs); false when there is no such block.
    pub fn delete_block(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE blocks SET deleted = 1, updated_at = ?2, synced = 0 WHERE id = ?1 AND deleted = 0",
            params![id, clock()],
        )? > 0)
    }

    /// Moves this machine's blocks from one project name to another (a folder that turned
    /// out to be a clone of another repository). Returns how many blocks moved.
    pub fn rename_project(&self, from: &str, to: &str, machine: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE blocks SET project = ?2, updated_at = ?4, synced = 0
              WHERE project = ?1 AND machine = ?3 AND deleted = 0",
            params![from, to, machine, clock()],
        )?)
    }

    /// Machines with blocks, most time first.
    pub fn machines(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT machine FROM blocks WHERE deleted = 0 GROUP BY machine ORDER BY SUM(end - start) DESC",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---------- sync ----------

    /// Changes made here that the sync files do not have yet.
    pub fn unsynced(&self) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, machine, project, editor, start, end, updated_at, deleted
               FROM blocks WHERE synced = 0 ORDER BY updated_at, id",
        )?;
        let rows = stmt.query_map([], |r| {
            let (id, at, deleted): (String, i64, bool) = (r.get(0)?, r.get(6)?, r.get(7)?);
            Ok(if deleted {
                Event::Delete { id, at }
            } else {
                Event::Put {
                    id,
                    machine: r.get(1)?,
                    project: r.get(2)?,
                    editor: r.get(3)?,
                    start: r.get(4)?,
                    end: r.get(5)?,
                    at,
                }
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks a change as written, unless the block changed again since.
    pub fn mark_synced(&self, uuid: &str, at: i64) -> Result<()> {
        self.conn
            .execute("UPDATE blocks SET synced = 1 WHERE uuid = ?1 AND updated_at = ?2", params![uuid, at])?;
        Ok(())
    }

    /// Applies a change from a sync file; the newest change to a block wins.
    /// Returns whether anything changed.
    pub fn apply(&self, e: &Event) -> Result<bool> {
        let current: Option<i64> = self
            .conn
            .query_row("SELECT updated_at FROM blocks WHERE uuid = ?1", params![e.id()], |r| r.get(0))
            .optional()?;
        if current.is_some_and(|at| at >= e.at()) {
            return Ok(false);
        }
        match e {
            Event::Put { id, machine, project, editor, start, end, at } => {
                self.conn.execute(
                    "INSERT INTO blocks (uuid, machine, project, editor, start, end, updated_at, deleted, synced)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 1)
                     ON CONFLICT(uuid) DO UPDATE SET machine = ?2, project = ?3, editor = ?4,
                         start = ?5, end = ?6, updated_at = ?7, deleted = 0, synced = 1",
                    params![id, machine, project, editor, start, end.max(start), at],
                )?;
            }
            // A deletion of a block not seen yet still leaves a mark, so an older copy
            // arriving later does not bring it back.
            Event::Delete { id, at } => {
                self.conn.execute(
                    "INSERT INTO blocks (uuid, project, start, end, updated_at, deleted, synced)
                     VALUES (?1, '', 0, 0, ?2, 1, 1)
                     ON CONFLICT(uuid) DO UPDATE SET updated_at = ?2, deleted = 1, synced = 1",
                    params![id, at],
                )?;
            }
        }
        Ok(true)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// How far a sync file has been read.
    pub fn file_offset(&self, path: &str) -> Result<u64> {
        let v: Option<i64> = self
            .conn
            .query_row("SELECT offset FROM sync_files WHERE path = ?1", params![path], |r| r.get(0))
            .optional()?;
        Ok(v.unwrap_or(0) as u64)
    }

    pub fn set_file_offset(&self, path: &str, offset: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_files (path, offset) VALUES (?1, ?2) ON CONFLICT(path) DO UPDATE SET offset = ?2",
            params![path, offset as i64],
        )?;
        Ok(())
    }
}

impl Sink for Store {
    fn open_block(&mut self, project: &str, editor: &str, start: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO blocks (uuid, machine, project, editor, start, end, updated_at)
             VALUES (lower(hex(randomblob(16))), ?1, ?2, ?3, ?4, ?4, ?5)",
            params![self.machine, project, editor, start, clock()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn set_end(&mut self, id: i64, end: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE blocks SET end = MAX(start, ?2), updated_at = MAX(updated_at, ?3), synced = 0 WHERE id = ?1",
            params![id, end, clock()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_clip_blocks_to_the_window() {
        let mut s = Store::in_memory().unwrap();
        let a = s.open_block("shop", "VS Code", 100).unwrap();
        s.set_end(a, 400).unwrap();
        let b = s.open_block("web", "VS Code", 500).unwrap();
        s.set_end(b, 560).unwrap();
        assert_eq!(s.totals(0, 1000).unwrap(), vec![("shop".into(), 300), ("web".into(), 60)]);
        assert_eq!(s.totals(300, 520).unwrap(), vec![("shop".into(), 100), ("web".into(), 20)]);
        assert_eq!(s.project_total("shop").unwrap(), 300);
    }

    #[test]
    fn edit_delete_and_overlap() {
        let mut s = Store::in_memory().unwrap();
        let a = s.open_block("shop", "VS Code", 100).unwrap();
        s.set_end(a, 200).unwrap();
        let b = s.open_block("web", "VS Code", 300).unwrap();
        s.set_end(b, 400).unwrap();

        assert_eq!(s.blocks(150, 350).unwrap().len(), 2);
        assert_eq!(s.blocks(200, 300).unwrap().len(), 0);
        assert_eq!(s.first_start().unwrap(), Some(100));

        assert!(!s.overlaps(a, 150, 350).unwrap(), "web is another project");
        let c = s.open_block("shop", "Claude Code", 150).unwrap();
        s.set_end(c, 350).unwrap();
        assert!(s.overlaps(a, 100, 300).unwrap(), "two shop blocks at once");
        s.delete_block(c).unwrap();
        assert!(!s.overlaps(a, 100, 300).unwrap());
        assert!(s.update_block(a, "codeclock", 120, 250).unwrap());
        assert_eq!(s.projects().unwrap(), vec![("codeclock".into(), 130), ("web".into(), 100)]);

        assert!(s.delete_block(b).unwrap());
        assert!(!s.delete_block(b).unwrap());
        assert!(!s.update_block(b, "x", 1, 2).unwrap());
    }

    #[test]
    fn migrates_a_version_0_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE blocks (id INTEGER PRIMARY KEY, project TEXT NOT NULL,
                 source TEXT NOT NULL DEFAULT 'editor', editor TEXT NOT NULL DEFAULT '',
                 start INTEGER NOT NULL, end INTEGER NOT NULL);
             INSERT INTO blocks (project, start, end) VALUES ('shop', 100, 200), ('web', 300, 400);",
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        let b = s.blocks(0, 1000).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].uuid.len(), 32);
        assert_ne!(b[0].uuid, b[1].uuid);
        assert!(!b[0].machine.is_empty());
        assert_eq!(s.unsynced().unwrap().len(), 2);
    }

    #[test]
    fn newest_change_wins_and_deletions_stick() {
        let s = Store::in_memory().unwrap();
        let put = |project: &str, at| Event::Put {
            id: "u1".into(),
            machine: "arch-linux".into(),
            project: project.into(),
            editor: "VS Code".into(),
            start: 100,
            end: 200,
            at,
        };
        assert!(s.apply(&put("shop", 10)).unwrap());
        assert!(!s.apply(&put("shop", 10)).unwrap(), "same change twice is ignored");
        assert!(!s.apply(&put("old", 5)).unwrap(), "older change loses");
        assert!(s.apply(&put("codeclock", 20)).unwrap());
        assert_eq!(s.projects().unwrap(), vec![("codeclock".into(), 100)]);
        assert!(s.unsynced().unwrap().is_empty(), "applied changes are not sent back");

        assert!(s.apply(&Event::Delete { id: "u1".into(), at: 30 }).unwrap());
        assert!(!s.apply(&put("shop", 25)).unwrap(), "an older copy does not undo the deletion");
        assert!(s.projects().unwrap().is_empty());

        // A deletion seen before the block itself.
        assert!(s.apply(&Event::Delete { id: "u2".into(), at: 50 }).unwrap());
        let mut late = put("shop", 40);
        if let Event::Put { id, .. } = &mut late {
            *id = "u2".into();
        }
        assert!(!s.apply(&late).unwrap());
        assert!(s.projects().unwrap().is_empty());
    }

    #[test]
    fn local_changes_are_listed_until_marked() {
        let mut s = Store::in_memory().unwrap();
        let a = s.open_block("shop", "VS Code", 100).unwrap();
        s.set_end(a, 200).unwrap();
        let ev = s.unsynced().unwrap();
        assert_eq!(ev.len(), 1);
        s.mark_synced(ev[0].id(), ev[0].at()).unwrap();
        assert!(s.unsynced().unwrap().is_empty());
        s.delete_block(a).unwrap();
        assert!(matches!(s.unsynced().unwrap()[..], [Event::Delete { .. }]));
    }

    #[test]
    fn rename_only_touches_this_machine() {
        let mut s = Store::in_memory().unwrap();
        s.set_machine("laptop-linux");
        let a = s.open_block("shop-test", "VS Code", 100).unwrap();
        s.set_end(a, 200).unwrap();
        s.apply(&Event::Put {
            id: "other".into(),
            machine: "arch-linux".into(),
            project: "shop-test".into(),
            editor: "VS Code".into(),
            start: 300,
            end: 350,
            at: 1,
        })
        .unwrap();
        assert_eq!(s.rename_project("shop-test", "shop", "laptop-linux").unwrap(), 1);
        assert_eq!(s.projects().unwrap(), vec![("shop".into(), 100), ("shop-test".into(), 50)]);
    }

    #[test]
    fn end_never_goes_before_start() {
        let mut s = Store::in_memory().unwrap();
        let a = s.open_block("shop", "VS Code", 100).unwrap();
        s.set_end(a, 50).unwrap();
        assert_eq!(s.project_total("shop").unwrap(), 0);
    }
}
