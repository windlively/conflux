use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection};

/// 表结构变更时递增，并在 `migrate` 中追加对应的迁移分支。
pub const SCHEMA_VERSION: i64 = 3;

/// 1 版结构：任务元数据 + 分片已提交偏移。
/// 跨会话续传靠的就是后者——恢复时按 segments 里的 downloaded 继续请求剩余区间。
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS tasks (
    id             TEXT    PRIMARY KEY,
    url            TEXT    NOT NULL,
    dir            TEXT    NOT NULL,
    file_name      TEXT    NOT NULL,
    save_path      TEXT    NOT NULL,
    status         TEXT    NOT NULL,
    total_bytes    INTEGER NOT NULL DEFAULT 0,
    supports_range INTEGER NOT NULL DEFAULT 0,
    probed         INTEGER NOT NULL DEFAULT 0,
    single_stream  INTEGER NOT NULL DEFAULT 0,
    connections    INTEGER NOT NULL,
    error          TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS segments (
    task_id    TEXT    NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL,
    start      INTEGER NOT NULL,
    len        INTEGER NOT NULL,
    downloaded INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, idx)
);
"#;

/// 2 版结构：任务区分协议（http/bt），BT 任务额外记住 info hash、磁力与种子文件。
/// 跨会话恢复 BT 任务就靠 info hash／磁力重新挂载。
const SCHEMA_V2: &str = r#"
ALTER TABLE tasks ADD COLUMN kind         TEXT NOT NULL DEFAULT 'http';
ALTER TABLE tasks ADD COLUMN info_hash    TEXT;
ALTER TABLE tasks ADD COLUMN magnet       TEXT;
ALTER TABLE tasks ADD COLUMN torrent_path TEXT;
"#;

/// 3 版结构：BT 任务勾选了哪几个文件（JSON 数组，NULL 表示全下）。
/// 重启后重新挂载要按它只下选中的文件。
const SCHEMA_V3: &str = r#"
ALTER TABLE tasks ADD COLUMN only_files TEXT;
"#;

/// 任务持久化。除了任务本身，还必须保存分片的已提交偏移，
/// 否则重启后无法从断点继续，只能整文件重下。
pub struct Store {
    conn: Mutex<Connection>,
}

/// 任务行。字段与 `tasks` 表一一对应，布尔值在库里存 0/1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: String,
    pub url: String,
    /// http／bt，见 download::types::TaskKind
    pub kind: String,
    pub dir: String,
    pub file_name: String,
    pub save_path: String,
    pub status: String,
    pub total_bytes: i64,
    pub supports_range: bool,
    pub probed: bool,
    pub single_stream: bool,
    pub connections: i64,
    pub error: Option<String>,
    /// BT：种子标识，重启后按它重新挂载
    pub info_hash: Option<String>,
    /// BT：用户填的磁力链接原文；种子文件任务则为空
    pub magnet: Option<String>,
    /// BT：来自 .torrent 文件时记下文件路径
    pub torrent_path: Option<String>,
    /// BT：勾选的文件序号（JSON 数组），NULL 表示全下
    pub only_files: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 分片行。`downloaded` 是已提交偏移：只有数据确实写入（并在周期性落库前 fsync 过）
/// 才会推进它，因此恢复时从该偏移继续是安全的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRecord {
    pub idx: i64,
    pub start: i64,
    pub len: i64,
    pub downloaded: i64,
}

const TASK_UPSERT: &str = "INSERT INTO tasks (id, url, kind, dir, file_name, save_path, status, \
     total_bytes, supports_range, probed, single_stream, connections, error, info_hash, magnet, \
     torrent_path, only_files, created_at, updated_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19) \
     ON CONFLICT(id) DO UPDATE SET url = excluded.url, kind = excluded.kind, dir = excluded.dir, \
     file_name = excluded.file_name, save_path = excluded.save_path, status = excluded.status, \
     total_bytes = excluded.total_bytes, supports_range = excluded.supports_range, \
     probed = excluded.probed, single_stream = excluded.single_stream, \
     connections = excluded.connections, error = excluded.error, info_hash = excluded.info_hash, \
     magnet = excluded.magnet, torrent_path = excluded.torrent_path, \
     only_files = excluded.only_files, updated_at = excluded.updated_at";

const SEGMENT_UPSERT: &str = "INSERT INTO segments (task_id, idx, start, len, downloaded) \
     VALUES (?1, ?2, ?3, ?4, ?5) \
     ON CONFLICT(task_id, idx) DO UPDATE SET start = excluded.start, len = excluded.len, \
     downloaded = excluded.downloaded";

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("无法创建数据目录 {}：{err}", parent.display()))?;
        }
        let mut conn = Connection::open(path)
            .map_err(|err| format!("无法打开数据库 {}：{err}", path.display()))?;
        configure(&conn)?;
        migrate(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    /// 内存库，供测试使用。
    pub fn open_in_memory() -> Result<Store, String> {
        let mut conn = Connection::open_in_memory().map_err(|err| format!("无法创建内存库：{err}"))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|err| format!("无法设置数据库参数：{err}"))?;
        migrate(&mut conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    pub fn schema_version(&self) -> Result<i64, String> {
        self.conn()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|err| format!("读取数据库版本失败：{err}"))
    }

    /// 写入或更新任务行。这里必须用 upsert 而不是 INSERT OR REPLACE：
    /// REPLACE 会先删后插，触发外键级联把该任务的 segments 一起删掉。
    pub fn save_task(&self, record: &TaskRecord) -> Result<(), String> {
        self.conn()
            .execute(
                TASK_UPSERT,
                params![
                    record.id,
                    record.url,
                    record.kind,
                    record.dir,
                    record.file_name,
                    record.save_path,
                    record.status,
                    record.total_bytes,
                    record.supports_range as i64,
                    record.probed as i64,
                    record.single_stream as i64,
                    record.connections,
                    record.error,
                    record.info_hash,
                    record.magnet,
                    record.torrent_path,
                    record.only_files,
                    record.created_at,
                    record.updated_at,
                ],
            )
            .map_err(|err| format!("写入任务失败：{err}"))?;
        Ok(())
    }

    pub fn save_segments(&self, task_id: &str, segments: &[SegmentRecord]) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn
            .transaction()
            .map_err(|err| format!("无法开始分片写入事务：{err}"))?;
        {
            let mut statement = tx
                .prepare(SEGMENT_UPSERT)
                .map_err(|err| format!("准备分片写入失败：{err}"))?;
            for segment in segments {
                statement
                    .execute(params![
                        task_id,
                        segment.idx,
                        segment.start,
                        segment.len,
                        segment.downloaded
                    ])
                    .map_err(|err| format!("写入分片失败：{err}"))?;
            }
        }
        tx.commit()
            .map_err(|err| format!("提交分片写入失败：{err}"))
    }

    pub fn delete_task(&self, id: &str) -> Result<(), String> {
        self.conn()
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])
            .map_err(|err| format!("删除任务失败：{err}"))?;
        Ok(())
    }

    pub fn load_tasks(&self) -> Result<Vec<TaskRecord>, String> {
        let conn = self.conn();
        let mut statement = conn
            .prepare(
                "SELECT id, url, kind, dir, file_name, save_path, status, total_bytes, \
                 supports_range, probed, single_stream, connections, error, info_hash, magnet, \
                 torrent_path, only_files, created_at, updated_at \
                 FROM tasks ORDER BY created_at, rowid",
            )
            .map_err(|err| format!("准备任务查询失败：{err}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(TaskRecord {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    kind: row.get(2)?,
                    dir: row.get(3)?,
                    file_name: row.get(4)?,
                    save_path: row.get(5)?,
                    status: row.get(6)?,
                    total_bytes: row.get(7)?,
                    supports_range: row.get::<_, i64>(8)? != 0,
                    probed: row.get::<_, i64>(9)? != 0,
                    single_stream: row.get::<_, i64>(10)? != 0,
                    connections: row.get(11)?,
                    error: row.get(12)?,
                    info_hash: row.get(13)?,
                    magnet: row.get(14)?,
                    torrent_path: row.get(15)?,
                    only_files: row.get(16)?,
                    created_at: row.get(17)?,
                    updated_at: row.get(18)?,
                })
            })
            .map_err(|err| format!("查询任务失败：{err}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("读取任务失败：{err}"))
    }

    pub fn load_segments(&self, task_id: &str) -> Result<Vec<SegmentRecord>, String> {
        let conn = self.conn();
        let mut statement = conn
            .prepare(
                "SELECT idx, start, len, downloaded FROM segments WHERE task_id = ?1 ORDER BY idx",
            )
            .map_err(|err| format!("准备分片查询失败：{err}"))?;
        let rows = statement
            .query_map(params![task_id], |row| {
                Ok(SegmentRecord {
                    idx: row.get(0)?,
                    start: row.get(1)?,
                    len: row.get(2)?,
                    downloaded: row.get(3)?,
                })
            })
            .map_err(|err| format!("查询分片失败：{err}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("读取分片失败：{err}"))
    }
}

fn configure(conn: &Connection) -> Result<(), String> {
    // WAL 让读写不互相阻塞；外键约束在 SQLite 里默认关闭，必须按连接开启，
    // 否则删除任务时会留下孤立的分片记录。
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
        .map_err(|err| format!("无法设置数据库参数：{err}"))
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|err| format!("读取数据库版本失败：{err}"))?;

    if version > SCHEMA_VERSION {
        // 用户可能装回了旧版本，继续用会损坏新结构
        return Err(format!(
            "数据库版本为 {version}，高于当前程序支持的 {SCHEMA_VERSION}，请升级应用后再打开"
        ));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    let tx = conn
        .transaction()
        .map_err(|err| format!("无法开始迁移事务：{err}"))?;
    if version < 1 {
        tx.execute_batch(SCHEMA_V1)
            .map_err(|err| format!("创建表结构失败：{err}"))?;
    }
    if version < 2 {
        tx.execute_batch(SCHEMA_V2)
            .map_err(|err| format!("升级到 2 版结构失败：{err}"))?;
    }
    if version < 3 {
        tx.execute_batch(SCHEMA_V3)
            .map_err(|err| format!("升级到 3 版结构失败：{err}"))?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|err| format!("写入数据库版本失败：{err}"))?;
    tx.commit().map_err(|err| format!("提交迁移失败：{err}"))
}
