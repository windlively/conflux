//! 应用内更新的下载与安装。
//!
//! tauri-plugin-updater 自带的下载是一根 TCP 流，从 GitHub 拉更新包通常只有几十
//! KB/s。这里改成和普通任务同一套思路：先探测服务器是否支持 Range，再按设置里的
//! 连接数并发拉分片，拼回整包后交给插件做签名校验与安装（校验不过不会替换应用）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;
use tokio::io::AsyncWriteExt;

/// 每片至少这么大：片切得太碎，握手与重试的开销会盖过并发收益
const MIN_PART_BYTES: u64 = 1024 * 1024;
/// 更新包最多开这么多连接，别把设置里的 32 条全用上
const MAX_CONNECTIONS: usize = 16;
/// 单片的失败重试次数
const PART_ATTEMPTS: usize = 2;

/// 进度事件：前端拿它画设置页里的进度条
#[derive(Clone, Serialize)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// 下载并安装更新。连接数由前端从设置里的「更新下载线程数」带过来。
#[tauri::command]
pub async fn install_update(app: AppHandle, connections: usize) -> Result<(), String> {
    let updater = app.updater().map_err(|err| err.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "已经是最新版本".to_string())?;

    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|err| err.to_string())?
        .join("updates");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| err.to_string())?;
    let path = dir.join(format!("conflux-{}.pkg", update.version));
    let url = update.download_url.to_string();

    download(&app, &url, &path, connections).await?;

    let bytes = tokio::fs::read(&path).await.map_err(|err| err.to_string())?;
    let _ = tokio::fs::remove_file(&path).await;

    // 插件先按配置里的公钥校验签名，再解包替换；校验不过这里就会报错
    update.install(&bytes).map_err(|err| err.to_string())?;

    // Windows 上安装器接管后进程已经退出了；macOS 需要自己重启到新版本
    #[cfg(not(windows))]
    {
        app.restart();
    }
    #[allow(unreachable_code)]
    Ok(())
}

async fn download(
    app: &AppHandle,
    url: &str,
    path: &Path,
    connections: usize,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| err.to_string())?;

    let (total, ranged) = probe(&client, url).await?;

    // 不知道大小、或者服务器不认 Range：老老实实单连接
    let total = match total {
        Some(total) if ranged && total > 0 => total,
        _ => return stream_to_file(app, &client, url, path, total).await,
    };

    let parts = plan_parts(total, connections);
    if parts.len() <= 1 {
        return stream_to_file(app, &client, url, path, Some(total)).await;
    }

    let done = Arc::new(AtomicU64::new(0));
    let ticker = spawn_progress(app, done.clone(), total);

    let mut tasks = Vec::with_capacity(parts.len());
    for (index, (start, end)) in parts.iter().copied().enumerate() {
        let client = client.clone();
        let url = url.to_string();
        let part = part_path(path, index);
        let done = done.clone();
        tasks.push(tokio::spawn(async move {
            fetch_range(&client, &url, start, end, &part, &done).await
        }));
    }

    let mut failed = None;
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => failed = failed.or(Some(err)),
            Err(err) => failed = failed.or(Some(err.to_string())),
        }
    }
    ticker.abort();

    // 某一片挂了就整包重来一次单连接：多一条路，比直接报错好
    if let Some(err) = failed {
        cleanup_parts(path);
        eprintln!("更新包分片下载失败（{err}），改用单连接重试");
        return stream_to_file(app, &client, url, path, Some(total)).await;
    }

    stitch(path, parts.len()).await?;
    let _ = app.emit(
        "update:progress",
        Progress {
            downloaded: total,
            total: Some(total),
        },
    );
    Ok(())
}

/// 探测总大小与是否支持 Range。用 `bytes=0-0` 而不是 HEAD：GitHub 的 CDN 对
/// 重定向后的 HEAD 有时不给 Content-Length，而 206 的 Content-Range 一定会带总长。
async fn probe(client: &reqwest::Client, url: &str) -> Result<(Option<u64>, bool), String> {
    let response = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|err| err.to_string())?;

    if response.status() == StatusCode::PARTIAL_CONTENT {
        let total = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|value| value.trim().parse::<u64>().ok());
        return Ok((total, total.is_some()));
    }

    Ok((response.content_length(), false))
}

/// 按连接数切片。片数不超过「总大小 / 1MB」，小更新包没必要开一堆连接。
fn plan_parts(total: u64, connections: usize) -> Vec<(u64, u64)> {
    let wanted = connections.clamp(1, MAX_CONNECTIONS) as u64;
    let count = (total / MIN_PART_BYTES).clamp(1, wanted);
    (0..count)
        .map(|index| {
            let start = total * index / count;
            let end = total * (index + 1) / count - 1;
            (start, end)
        })
        .collect()
}

async fn fetch_range(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    path: &Path,
    done: &AtomicU64,
) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..PART_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        match fetch_range_once(client, url, start, end, path, done).await {
            Ok(()) => return Ok(()),
            Err(err) => last = err,
        }
    }
    Err(last)
}

async fn fetch_range_once(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    path: &Path,
    done: &AtomicU64,
) -> Result<(), String> {
    let response = client
        .get(url)
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(format!("分片请求被拒绝（HTTP {}）", response.status()));
    }

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|err| err.to_string())?;
    let mut stream = response.bytes_stream();
    let mut written = 0u64;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                if let Err(err) = file.write_all(&chunk).await {
                    done.fetch_sub(written, Ordering::Relaxed);
                    return Err(err.to_string());
                }
                done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                written += chunk.len() as u64;
            }
            Err(err) => {
                // 这一片白下了：把已经算进去的进度扣回去，重试才不会超过 100%
                done.fetch_sub(written, Ordering::Relaxed);
                return Err(err.to_string());
            }
        }
    }
    file.flush().await.map_err(|err| err.to_string())?;
    Ok(())
}

/// 单连接兜底：不支持 Range、或者分片失败时走这条
async fn stream_to_file(
    app: &AppHandle,
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    total: Option<u64>,
) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("下载更新包失败（HTTP {}）", response.status()));
    }

    let total = total.or(response.content_length());
    let done = Arc::new(AtomicU64::new(0));
    let ticker = total.map(|total| spawn_progress(app, done.clone(), total));

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|err| err.to_string())?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| err.to_string())?;
        file.write_all(&chunk).await.map_err(|err| err.to_string())?;
        done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }
    file.flush().await.map_err(|err| err.to_string())?;
    if let Some(ticker) = ticker {
        ticker.abort();
    }
    let _ = app.emit(
        "update:progress",
        Progress {
            downloaded: done.load(Ordering::Relaxed),
            total,
        },
    );
    Ok(())
}

/// 每 200ms 推一次进度：事件太密只会让前端白忙
fn spawn_progress(
    app: &AppHandle,
    done: Arc<AtomicU64>,
    total: u64,
) -> tokio::task::JoinHandle<()> {
    let app = app.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        loop {
            ticker.tick().await;
            let _ = app.emit(
                "update:progress",
                Progress {
                    downloaded: done.load(Ordering::Relaxed),
                    total: Some(total),
                },
            );
        }
    })
}

async fn stitch(path: &Path, parts: usize) -> Result<(), String> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|err| err.to_string())?;
    for index in 0..parts {
        let part = part_path(path, index);
        let mut source = tokio::fs::File::open(&part)
            .await
            .map_err(|err| err.to_string())?;
        tokio::io::copy(&mut source, &mut file)
            .await
            .map_err(|err| err.to_string())?;
        let _ = tokio::fs::remove_file(&part).await;
    }
    file.flush().await.map_err(|err| err.to_string())?;
    Ok(())
}

fn part_path(path: &Path, index: usize) -> PathBuf {
    path.with_extension(format!("part{index}"))
}

fn cleanup_parts(path: &Path) {
    for index in 0..MAX_CONNECTIONS {
        let _ = std::fs::remove_file(part_path(path, index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_cover_the_whole_file_without_gaps() {
        let parts = plan_parts(16 * 1024 * 1024, 8);
        assert_eq!(parts.len(), 8);
        assert_eq!(parts.first().unwrap().0, 0);
        assert_eq!(parts.last().unwrap().1, 16 * 1024 * 1024 - 1);
        for pair in parts.windows(2) {
            assert_eq!(pair[0].1 + 1, pair[1].0);
        }
    }

    #[test]
    fn small_files_stay_on_one_connection() {
        // 不到 1 MB 的包切了也没意义
        assert_eq!(plan_parts(600 * 1024, 8).len(), 1);
        // 连接数不能超过上限，也不能超过「每片 1MB」算出来的片数
        assert_eq!(plan_parts(5 * 1024 * 1024, 32).len(), 5);
        assert_eq!(plan_parts(64 * 1024 * 1024, 32).len(), MAX_CONNECTIONS);
    }

    #[test]
    fn part_paths_do_not_collide() {
        let path = Path::new("/tmp/conflux-0.3.0.pkg");
        assert_ne!(part_path(path, 0), part_path(path, 1));
        assert_ne!(part_path(path, 1), path);
    }
}
