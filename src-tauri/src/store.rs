use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

/// 表结构变更时递增，并在 `migrate` 中追加对应的迁移分支。
pub const SCHEMA_VERSION: i64 = 1;

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

/// 任务持久化。除了任务本身，还必须保存分片的已提交偏移，
/// 否则重启后无法从断点继续，只能整文件重下。
pub struct Store {
    conn: Mutex<Connection>,
}

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
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|err| format!("写入数据库版本失败：{err}"))?;
    tx.commit().map_err(|err| format!("提交迁移失败：{err}"))
}
