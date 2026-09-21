//! BT／磁力：进程内 librqbit 会话。
//!
//! 全应用共用一个会话，BT 任务是「磁力 + 输出目录」。暂停＝把种子停掉但保留数据，
//! 继续＝按同一 info hash 重新挂载，librqbit 会核对磁盘上已有的分片接着下，
//! 所以暂停/继续不需要额外的会话持久化。

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use librqbit::api::TorrentIdOrHash;
use librqbit::dht::Id20;
use librqbit::{
    torrent_from_bytes, AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig,
    ListenerMode, ListenerOptions, ListOnlyResponse, ManagedTorrent, Magnet, Session,
    SessionOptions, TorrentStatsState, ValidatedTorrentMetaV1Info,
};
use tokio_util::sync::CancellationToken;

use super::engine::{Outcome, ProgressFlush, ProgressSink};
use super::path::sanitize_file_name;
use super::task::{StopKind, Task};
use super::types::{BtStats, TaskStatus, TorrentFile, TorrentPreview};

/// 进度轮询间隔，librqbit 的统计是同步读内存，跟上界面绰绰有余
const TICK: Duration = Duration::from_millis(250);
/// 进度落库要 fsync，不必跟轮询一样勤
const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// 磁力链接里我们关心的部分。
pub struct MagnetInfo {
    pub info_hash: String,
    pub name: Option<String>,
}

pub fn parse_magnet(url: &str) -> Result<MagnetInfo, String> {
    let magnet = Magnet::parse(url).map_err(|err| format!("磁力链接无法解析：{err}"))?;
    let info_hash = magnet
        .as_id20()
        .ok_or_else(|| "磁力链接里没有 BT v1 的 info hash（btih）".to_string())?;
    Ok(MagnetInfo {
        info_hash: info_hash.as_string(),
        name: magnet.name,
    })
}

/// 种子文件自带元数据：与磁力不同，入队时就知道名字、总大小与文件清单，不必等 peer。
pub fn parse_torrent_file(path: &Path) -> Result<TorrentPreview, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("无法读取种子文件：{err}"))?;
    let meta = torrent_from_bytes(&bytes).map_err(|err| format!("种子文件无法解析：{err}"))?;
    let info_hash = meta.info_hash.as_string();
    // validate 顺带做编码探测与路径穿越检查，名字与文件长度都从它取
    let info = meta
        .info
        .data
        .validate()
        .map_err(|err| format!("种子文件内容不合法：{err}"))?;
    Ok(TorrentPreview {
        info_hash,
        name: info.name().map(|name| name.to_string()),
        total_bytes: info.iter_file_lengths().sum(),
        files: files_of(&info),
    })
}

/// 元数据到手后从会话里取文件清单（`ManagedTorrentHandle` 就是 `Arc<ManagedTorrent>`）。
fn files_from_metadata(handle: &Arc<ManagedTorrent>) -> Vec<TorrentFile> {
    handle
        .with_metadata(|meta| files_of(&meta.info))
        .unwrap_or_default()
}

fn files_of<BufType: AsRef<[u8]>>(info: &ValidatedTorrentMetaV1Info<BufType>) -> Vec<TorrentFile> {
    info.iter_file_details()
        .enumerate()
        .map(|(index, details)| TorrentFile {
            index,
            name: details.filename.to_string(),
            length: details.len,
            selected: true,
            downloaded: 0,
        })
        .collect()
}

/// 只取元数据、不把种子加进会话：界面上的「预览」与「磁力先解析再挂载」都走这里。
/// 磁力本身不含清单，所以这一步仍要联网找 peer；预览带超时，下载路径则一直等下去。
async fn list_only(
    runtime: &BtRuntime,
    magnet: &str,
    timeout: Option<Duration>,
) -> Result<ListOnlyResponse, String> {
    let options = AddTorrentOptions {
        list_only: true,
        initial_peers: (!runtime.extra_peers.is_empty()).then(|| runtime.extra_peers.clone()),
        ..Default::default()
    };
    let resolving = runtime
        .session()
        .add_torrent(AddTorrent::from_url(magnet.to_string()), Some(options));
    let added = match timeout {
        Some(timeout) => tokio::time::timeout(timeout, resolving).await.map_err(|_| {
            format!(
                "{} 秒内没能从任何 peer 拿到元数据；可以先直接开始下载，任务会在后台继续找 peer",
                timeout.as_secs()
            )
        })?,
        None => resolving.await,
    }
    .map_err(|err| format!("无法解析这个磁力：{err}"))?;

    match added {
        AddTorrentResponse::ListOnly(list) => Ok(list),
        _ => Err("会话没有返回「仅列表」结果".to_string()),
    }
}

fn preview_of(list: &ListOnlyResponse) -> TorrentPreview {
    TorrentPreview {
        info_hash: list.info_hash.as_string(),
        name: list.info.name().map(|name| name.to_string()),
        total_bytes: list.info.iter_file_lengths().sum(),
        files: files_of(&list.info),
    }
}

/// 只取元数据、不建任务也不落盘：给「预览磁力内容」用。
pub async fn preview_magnet(
    runtime: &BtRuntime,
    magnet: &str,
    timeout: Duration,
) -> Result<TorrentPreview, String> {
    let list = list_only(runtime, magnet, Some(timeout)).await?;
    Ok(preview_of(&list))
}

/// 落盘位置。librqbit 的惯例是单文件种子直接放输出目录、多文件种子在下面再套一层
/// 以种子名命名的目录，但它只在「没给输出目录」时才这么算；我们按任务指定了输出目录，
/// 这一层就得自己算出来，否则多个种子的文件会混在用户选的同一个目录里。
fn output_folder_for(dir: &Path, name: Option<&str>, file_count: usize) -> PathBuf {
    if file_count >= 2 {
        if let Some(folder) = name.map(sanitize_file_name).filter(|name| !name.is_empty()) {
            return dir.join(folder);
        }
    }
    dir.to_path_buf()
}

/// 挂载一个种子要用的全部参数。
struct Mount {
    add: AddTorrent<'static>,
    output_folder: PathBuf,
    initial_peers: Option<Vec<SocketAddr>>,
    /// 挂载前就知道的清单：种子文件直接解析，磁力靠这一次元数据交换
    preview: TorrentPreview,
}

/// 定下「从哪儿挂、落到哪个目录、初始 peer 有哪些」。
/// 磁力要先解析一次元数据：清单既是给界面看的，也决定要不要套那层目录。
async fn prepare(task: &Task, runtime: &BtRuntime, target: Target) -> Result<Mount, String> {
    let dir = task.inner.lock().unwrap().dir.clone();
    let mut extra_peers = runtime.extra_peers.clone();
    let (add, preview) = match target {
        Target::TorrentFile(path) => {
            let preview = parse_torrent_file(&path)?;
            let add = AddTorrent::from_local_filename(&path.to_string_lossy())
                .map_err(|err| format!("无法读取种子文件：{err}"))?;
            (add, preview)
        }
        Target::Magnet(magnet) => {
            let mut resolved = list_only(runtime, &magnet, None).await?;
            let preview = preview_of(&resolved);
            // 解析时见过的 peer 接着用，不然这一步白连了
            extra_peers.append(&mut resolved.seen_peers);
            (AddTorrent::from_bytes(resolved.torrent_bytes), preview)
        }
    };
    let output_folder = output_folder_for(&dir, preview.name.as_deref(), preview.files.len());
    Ok(Mount {
        add,
        output_folder,
        initial_peers: (!extra_peers.is_empty()).then_some(extra_peers),
        preview,
    })
}

/// 会话里是否已经挂着这个种子：暂停后继续时靠它直接挂回去，不必再解析一次元数据。
fn managed_by_info_hash(runtime: &BtRuntime, task: &Task) -> Option<Arc<ManagedTorrent>> {
    let info_hash = task.inner.lock().unwrap().bt.as_ref()?.info_hash.clone();
    let id: Id20 = info_hash.parse().ok()?;
    runtime.session().get(TorrentIdOrHash::Hash(id))
}

/// 把种子挂到会话上，返回它的句柄。
async fn add_to_session(
    task: &Task,
    runtime: &BtRuntime,
    mount: Mount,
) -> Result<Arc<ManagedTorrent>, String> {
    let options = AddTorrentOptions {
        output_folder: Some(mount.output_folder.to_string_lossy().to_string()),
        // 续传要求允许写已有文件，librqbit 会核对分片后接着下
        overwrite: true,
        // 用户在弹窗里勾过的文件，挂载时就带上；没勾过（None）表示全下
        only_files: task.only_files(),
        initial_peers: mount.initial_peers,
        ..Default::default()
    };
    match runtime.session().add_torrent(mount.add, Some(options)).await {
        Ok(AddTorrentResponse::Added(_, handle))
        | Ok(AddTorrentResponse::AlreadyManaged(_, handle)) => Ok(handle),
        Ok(AddTorrentResponse::ListOnly(_)) => Err("会话返回了意外的「仅列表」结果".to_string()),
        Err(err) => Err(format!("无法挂载种子：{err}")),
    }
}

/// 这个任务要从哪儿挂载：磁力链接，还是本地种子文件。
enum Target {
    Magnet(String),
    TorrentFile(std::path::PathBuf),
}

fn target_of(task: &Task) -> Option<Target> {
    let inner = task.inner.lock().unwrap();
    let bt = inner.bt.as_ref()?;
    if let Some(magnet) = &bt.magnet {
        return Some(Target::Magnet(magnet.clone()));
    }
    bt.torrent_path
        .as_ref()
        .map(|path| Target::TorrentFile(std::path::PathBuf::from(path)))
}

pub struct BtRuntime {
    session: Arc<Session>,
    /// 夹具用：磁力自身不带 peer 地址，离线测试直接连这些地址；生产为空
    extra_peers: Vec<SocketAddr>,
}

impl BtRuntime {
    /// 生产会话：开 DHT 与 tracker，在全部网卡上监听随机端口。
    pub async fn new(default_dir: PathBuf) -> Result<Arc<Self>, String> {
        Self::build(default_dir, Vec::new(), false).await
    }

    /// 测试夹具：关掉 DHT、tracker 与局域网发现，只在回环地址监听，
    /// peer 只连显式给出的地址，全程不出本机。
    pub async fn local(
        default_dir: PathBuf,
        extra_peers: Vec<SocketAddr>,
    ) -> Result<Arc<Self>, String> {
        Self::build(default_dir, extra_peers, true).await
    }

    async fn build(
        default_dir: PathBuf,
        extra_peers: Vec<SocketAddr>,
        local: bool,
    ) -> Result<Arc<Self>, String> {
        let options = SessionOptions {
            // 每次启动重新引导 DHT 即可，不往磁盘写路由表
            dht: (!local).then(|| DhtSessionConfig {
                persistence: None,
                ..Default::default()
            }),
            disable_trackers: local,
            disable_local_service_discovery: local,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: if local {
                    (Ipv4Addr::LOCALHOST, 0).into()
                } else {
                    SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
                },
                ipv4_only: local,
                ..Default::default()
            }),
            client_name_and_version: Some("Conflux/0.1".to_string()),
            ..Default::default()
        };
        let session = Session::new_with_opts(default_dir, options)
            .await
            .map_err(|err| format!("无法启动 BT 会话：{err}"))?;
        Ok(Arc::new(BtRuntime {
            session,
            extra_peers,
        }))
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// 监听端口，绑定完成后才有值；测试夹具拿它当「做种者的地址」。
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.session.listen_addr()
    }
}

fn set_status(task: &Task, status: TaskStatus) {
    task.inner.lock().unwrap().status = status;
}

/// 种子一停连接就断了，别把上一次刷新到的 peer 数留在界面上
fn clear_peers(task: &Task) {
    let mut inner = task.inner.lock().unwrap();
    if let Some(stats) = inner.bt_stats.as_mut() {
        stats.peers_connected = 0;
    }
}

fn fail(task: &Task, sink: &ProgressSink, message: String) -> Outcome {
    task.set_error(message.clone());
    set_status(task, TaskStatus::Failed);
    sink(task.snapshot());
    Outcome::Failed(message)
}

/// 挂载种子并跟到结束。返回时任务一定不在会话里跑了：
/// 完成与暂停都会把种子停掉，取消带删文件时连数据一起清掉。
pub async fn run(
    task: Arc<Task>,
    runtime: Arc<BtRuntime>,
    sink: ProgressSink,
    cancel: CancellationToken,
    flush: Option<ProgressFlush>,
) -> Outcome {
    let Some(target) = target_of(&task) else {
        return fail(
            &task,
            &sink,
            "这个 BT 任务既没有磁力链接也没有种子文件".to_string(),
        );
    };

    set_status(&task, TaskStatus::Probing);
    sink(task.snapshot());

    // 会话里已经挂着这个种子（暂停后继续）：直接挂回去，不必再解析一次元数据
    let mounted = match managed_by_info_hash(&runtime, &task) {
        Some(handle) => Ok(handle),
        None => {
            // 磁力要先联网取元数据，这一步可能要等一会儿，期间也要能被暂停/取消
            let prepared = tokio::select! {
                prepared = prepare(&task, &runtime, target) => prepared,
                _ = cancel.cancelled() => {
                    task.zero_speeds();
                    clear_peers(&task);
                    sink(task.snapshot());
                    if let Some(flush) = &flush {
                        flush();
                    }
                    return Outcome::Cancelled;
                }
            };
            match prepared {
                Err(message) => return fail(&task, &sink, message),
                Ok(mount) => {
                    // 清单先落给任务：会话还在核对已有分片时，界面就能列出文件、也能改勾选
                    task.set_torrent_files(mount.preview.files.clone());
                    let name = mount
                        .preview
                        .name
                        .clone()
                        .unwrap_or_else(|| task.file_name());
                    task.apply_torrent_meta(
                        &name,
                        mount.preview.total_bytes,
                        Some(mount.preview.info_hash.clone()),
                    );
                    sink(task.snapshot());
                    add_to_session(&task, &runtime, mount).await
                }
            }
        }
    };
    let handle = match mounted {
        Ok(handle) => handle,
        Err(message) => return fail(&task, &sink, message),
    };
    // 暂停过的任务重新挂载后仍是暂停态，这里把它放回运行
    if handle.is_paused() {
        if let Err(err) = runtime.session().unpause(&handle).await {
            return fail(&task, &sink, format!("无法继续这个种子：{err}"));
        }
    }

    let mut last_flush = Instant::now();
    let mut metadata_seen = false;
    // 会话当前算哪些文件：暂停期间用户可能改过勾选，以它为准决定要不要同步过去
    let mut applied_only_files = handle.only_files();
    loop {
        let stats = handle.stats();

        if stats.total_bytes > 0 && !metadata_seen {
            metadata_seen = true;
            let name = handle.name().unwrap_or_else(|| task.file_name());
            task.set_torrent_files(files_from_metadata(&handle));
            task.apply_torrent_meta(
                &name,
                stats.total_bytes,
                Some(handle.info_hash().as_string()),
            );
            set_status(&task, TaskStatus::Downloading);
            // 元数据一到就落库，重启后按 info hash 就能重新挂上
            if let Some(flush) = &flush {
                flush();
            }
        }
        // 用户改了勾选：交给会话应用，改不动就记下来别再每拍重试
        let wanted = task.only_files();
        if metadata_seen && wanted != applied_only_files {
            let file_count = task.inner.lock().unwrap().bt_files.len();
            let selection: std::collections::HashSet<usize> = match &wanted {
                Some(indices) => indices.iter().copied().collect(),
                None => (0..file_count).collect(),
            };
            if let Err(err) = runtime.session().update_only_files(&handle, &selection).await {
                eprintln!("更新勾选失败：{err}");
            }
            applied_only_files = wanted;
        }
        if metadata_seen {
            task.set_progress(stats.progress_bytes, stats.total_bytes);
            task.set_file_progress(&stats.file_progress);
            // peer 数取自会话的统计快照：断种时两者都是 0，界面据此提示「没有连上任何 peer」
            let peers = stats.live.as_ref().map(|live| &live.snapshot.peer_stats);
            task.set_bt_stats(BtStats {
                peers_connected: peers.map(|peers| peers.live).unwrap_or(0),
                peers_seen: peers.map(|peers| peers.seen).unwrap_or(0),
                uploaded_bytes: stats.uploaded_bytes,
            });
        }
        sink(task.snapshot());

        if matches!(stats.state, TorrentStatsState::Error) {
            let message = stats
                .error
                .clone()
                .unwrap_or_else(|| "BT 会话报告了未知错误".to_string());
            let _ = runtime.session().pause(&handle).await;
            return fail(&task, &sink, message);
        }

        if stats.finished {
            // 完成即停：做种比例与上传限速属于设置项，7c 再放进来
            let _ = runtime.session().pause(&handle).await;
            {
                let mut inner = task.inner.lock().unwrap();
                inner.status = TaskStatus::Completed;
                inner.downloaded = inner.total;
            }
            task.zero_speeds();
            clear_peers(&task);
            sink(task.snapshot());
            if let Some(flush) = &flush {
                flush();
            }
            return Outcome::Completed;
        }

        if last_flush.elapsed() >= FLUSH_EVERY {
            if let Some(flush) = &flush {
                flush();
            }
            last_flush = Instant::now();
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                let (removed, delete_file) = {
                    let inner = task.inner.lock().unwrap();
                    (
                        inner.removed,
                        matches!(inner.stop, Some(StopKind::Cancel { delete_file: true })),
                    )
                };
                if removed || delete_file {
                    // 交给会话删：它才知道这个种子到底落了哪些文件；
                    // 不带删文件时数据留在原地，下次挂载还能续
                    if let Err(err) = runtime
                        .session()
                        .delete(TorrentIdOrHash::Id(handle.id()), delete_file)
                        .await
                    {
                        eprintln!("从会话摘除种子失败：{err}");
                    }
                } else if let Err(err) = runtime.session().pause(&handle).await {
                    eprintln!("暂停种子失败：{err}");
                }
                task.zero_speeds();
                clear_peers(&task);
                sink(task.snapshot());
                // 收尾这一下的状态（已暂停／已取消）与断点进度必须落库，
                // 否则重启后恢复出来的是上一次周期性落库的旧状态
                if let Some(flush) = &flush {
                    flush();
                }
                return Outcome::Cancelled;
            }
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}
