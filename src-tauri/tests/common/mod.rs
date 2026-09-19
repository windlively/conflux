//! 测试公共辅助：临时目录守卫。

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时目录守卫：测试结束（含断言失败触发 panic 展开）时自动删除整个目录，
/// 测试用例不必再手动收尾。可以当 `PathBuf` 用，需要一份独立的所有权时
/// 显式调用 `to_path_buf()`，避免与守卫本身的生命周期混淆。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 在系统临时目录下创建 `conflux-{tag}-{纳秒时间戳}`。
    pub fn new(tag: &str) -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("conflux-{tag}-{nanos}"));
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
