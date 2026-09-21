//! 数据库结构测试：建表、版本迁移、外键约束，以及分片数据能否支撑跨会话续传。

use conflux_lib::store::{Store, SCHEMA_VERSION};

mod common;

use common::TempDir;

fn table_names(store: &Store) -> Vec<String> {
    let conn = store.conn();
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("查询表结构失败");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("读取表名失败")
        .map(|row| row.expect("行读取失败"))
        .filter(|name| !name.starts_with("sqlite_"))
        .collect()
}

const INSERT_TASK: &str = "INSERT INTO tasks (id, url, dir, file_name, save_path, status, \
     total_bytes, supports_range, probed, single_stream, connections, error, created_at, updated_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)";

#[test]
fn fresh_database_creates_expected_schema() {
    let dir = TempDir::new("store-fresh");
    let path = dir.join("conflux.db");
    let store = Store::open(&path).expect("打开数据库应成功");

    assert_eq!(
        table_names(&store),
        vec!["segments".to_string(), "tasks".to_string()]
    );
    assert_eq!(store.schema_version().expect("读取版本失败"), SCHEMA_VERSION);
    assert!(path.exists(), "数据库文件应已创建");
}

#[test]
fn reopening_keeps_existing_rows() {
    let dir = TempDir::new("store-reopen");
    let path = dir.join("conflux.db");
    {
        let store = Store::open(&path).expect("首次打开应成功");
        store
            .conn()
            .execute(
                INSERT_TASK,
                rusqlite::params![
                    "t1",
                    "https://example.com/a.bin",
                    "/tmp",
                    "a.bin",
                    "/tmp/a.bin",
                    "paused",
                    100i64,
                    1,
                    1,
                    0,
                    4,
                    None::<String>,
                    1i64,
                    1i64
                ],
            )
            .expect("插入任务失败");
    }

    let store = Store::open(&path).expect("再次打开应成功");
    let count: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("统计任务失败");
    assert_eq!(count, 1, "重新打开不应清空已有数据");
    assert_eq!(store.schema_version().expect("读取版本失败"), SCHEMA_VERSION);
}

/// 旧版本应用留下的 1 版结构：迁移测试要写死当时的形状，不能跟着新结构走。
const OLD_V1_SCHEMA: &str = r#"
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

/// 老库要能原样打开：迁移补齐新列，老任务的数据一个不丢，
/// 并且按新结构读得出来（缺列的话 load_tasks 直接报错）。
#[test]
fn opening_an_old_database_migrates_without_losing_tasks() {
    let dir = TempDir::new("store-migrate");
    let path = dir.join("conflux.db");

    {
        let conn = rusqlite::Connection::open(&path).expect("创建旧库失败");
        conn.execute_batch(OLD_V1_SCHEMA).expect("建旧表失败");
        conn.execute(
            INSERT_TASK,
            rusqlite::params![
                "old-1",
                "https://example.com/old.bin",
                "/tmp",
                "old.bin",
                "/tmp/old.bin",
                "paused",
                4096i64,
                1,
                1,
                0,
                4,
                None::<String>,
                1i64,
                2i64
            ],
        )
        .expect("插入旧任务失败");
        conn.pragma_update(None, "user_version", 1i64)
            .expect("写入旧版本号失败");
    }

    let store = Store::open(&path).expect("打开旧库应自动迁移");
    assert_eq!(store.schema_version().expect("读取版本失败"), SCHEMA_VERSION);

    let tasks = store.load_tasks().expect("迁移后应能按新结构读回任务");
    assert_eq!(tasks.len(), 1, "迁移不该丢任务");
    let record = &tasks[0];
    assert_eq!(record.id, "old-1");
    assert_eq!(record.total_bytes, 4096);
    assert_eq!(record.status, "paused");
    assert_eq!(record.kind, "http", "老任务没有协议列，按 http 补齐");
    assert!(record.only_files.is_none(), "老任务没有勾选记录");
    assert!(record.magnet.is_none() && record.info_hash.is_none());
}

#[test]
fn segments_follow_their_task() {
    let store = Store::open_in_memory().expect("创建内存库应成功");
    let conn = store.conn();

    conn.execute(
        INSERT_TASK,
        rusqlite::params![
            "t1",
            "https://example.com/a.bin",
            "/tmp",
            "a.bin",
            "/tmp/a.bin",
            "downloading",
            400i64,
            1,
            1,
            0,
            4,
            None::<String>,
            1i64,
            1i64
        ],
    )
    .expect("插入任务失败");
    conn.execute(
        "INSERT INTO segments (task_id, idx, start, len, downloaded) VALUES ('t1', 0, 0, 100, 40)",
        [],
    )
    .expect("插入分片失败");

    let orphan = conn.execute(
        "INSERT INTO segments (task_id, idx, start, len, downloaded) VALUES ('missing', 0, 0, 100, 0)",
        [],
    );
    assert!(orphan.is_err(), "外键约束应拒绝指向不存在任务的分片");

    conn.execute("DELETE FROM tasks WHERE id = 't1'", [])
        .expect("删除任务失败");
    let left: i64 = conn
        .query_row("SELECT COUNT(*) FROM segments", [], |row| row.get(0))
        .expect("统计分片失败");
    assert_eq!(left, 0, "删除任务应级联删除其分片");
}

#[test]
fn segments_cover_the_whole_file_and_expose_resume_offset() {
    let store = Store::open_in_memory().expect("创建内存库应成功");
    let conn = store.conn();

    const TOTAL: i64 = 167_772_160;
    const SEGMENT: i64 = 41_943_040;
    const PARTIAL: i64 = 12_345_678;

    conn.execute(
        INSERT_TASK,
        rusqlite::params![
            "t1",
            "http://127.0.0.1:8787/test.bin",
            "/tmp/dl",
            "test.bin",
            "/tmp/dl/test.bin",
            "paused",
            TOTAL,
            1,
            1,
            0,
            4,
            None::<String>,
            1_758_000_000_000i64,
            1_758_000_001_000i64
        ],
    )
    .expect("插入任务失败");

    for (idx, downloaded) in [(0i64, SEGMENT), (1, PARTIAL), (2, 0), (3, 0)] {
        conn.execute(
            "INSERT INTO segments (task_id, idx, start, len, downloaded) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["t1", idx, idx * SEGMENT, SEGMENT, downloaded],
        )
        .expect("插入分片失败");
    }

    let (count, covered, done): (i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(len), 0), COALESCE(SUM(downloaded), 0) \
             FROM segments WHERE task_id = ?1",
            rusqlite::params!["t1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("汇总分片失败");

    assert_eq!(count, 4);
    assert_eq!(covered, TOTAL, "分片区间必须刚好铺满整个文件");
    assert_eq!(
        done,
        SEGMENT + PARTIAL,
        "已提交偏移应能直接汇总出总进度与各分片续传起点"
    );

    let (status, total, name): (String, i64, String) = conn
        .query_row(
            "SELECT status, total_bytes, file_name FROM tasks WHERE id = ?1",
            rusqlite::params!["t1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("读回任务失败");
    assert_eq!(status, "paused");
    assert_eq!(total, TOTAL);
    assert_eq!(name, "test.bin");
}
