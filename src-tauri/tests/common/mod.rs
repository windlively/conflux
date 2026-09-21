//! 测试公共辅助：临时目录守卫。

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时目录守卫：测试结束（含断言失败触发 panic 展开）时自动删除整个目录，
/// 测试用例不必再手动收尾。可以当 `PathBuf` 用，需要一份独立的所有权时
/// 显式调用 `to_path_buf()`，避免与守卫本身的生命周期混淆。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 在系统临时目录下创建 `conflux-{tag}-{进程号}-{纳秒时间戳}-{序号}`。
    /// 只用时间戳不够：同一个测试文件里的用例是并发跑的，同一纳秒撞上同一个目录时，
    /// 先结束的那个会把后来者的文件连同数据库一起删掉。
    pub fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "conflux-{tag}-{}-{nanos}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
}

impl Deref for TempDir {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
