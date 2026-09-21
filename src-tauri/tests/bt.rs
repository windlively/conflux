//! BT／磁力端到端测试：本机「做种 + 下载」夹具。
//!
//! 用同一个 librqbit 起两个会话，一个给源文件做种，另一个当下载端；
//! 两端都关掉 DHT 与 tracker，下载端只连做种者的回环地址，
//! 于是「磁力 → 元数据交换 → 分片下载 → 落库」整条链路都跑在本机、不依赖外网。

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use conflux_lib::download::bt::BtRuntime;
use conflux_lib::download::manager::{DownloadManager, EventSink, ManagerEvent};
use conflux_lib::download::types::{TaskKind, TaskStatus, TaskView};
use conflux_lib::store::Store;
use librqbit::limits::LimitsConfig;
use librqbit::spawn_utils::BlockingSpawner;
use librqbit::{
    create_torrent, AddTorrent, AddTorrentOptions, CreateTorrentOptions, CreateTorrentResult,
};

mod common;

use common::TempDir;

/// 夹具用的源文件大小：跨过几个分片，又不至于让测试等太久。
const PAYLOAD_LEN: usize = 3 * 1024 * 1024 + 12345;

struct Harness {
    root: TempDir,
    payload: Vec<u8>,
    torrent: CreateTorrentResult,
    /// 做种会话要活到测试结束，Drop 掉就没人上传了
    _seeding: Arc<BtRuntime>,
}

/// 多文件夹具：一个目录三个文件，用来验证预览与「只下勾选的」。
struct MultiHarness {
    root: TempDir,
    /// 文件名与内容；种子里的顺序不保证，断言按名字查
    contents: Vec<(String, Vec<u8>)>,
    /// 多文件种子下载后会多一层以目录名命名的子目录
    folder: String,
    torrent: CreateTorrentResult,
    _seeding: Arc<BtRuntime>,
}

impl MultiHarness {
    fn file(&self, name: &str) -> &(String, Vec<u8>) {
        self.contents
            .iter()
            .find(|(file_name, _)| file_name == name)
            .unwrap_or_else(|| panic!("夹具里没有 {name}"))
    }

    fn content(&self, name: &str) -> &[u8] {
        &self.file(name).1
    }

    fn total_bytes(&self) -> u64 {
        self.contents.iter().map(|(_, data)| data.len() as u64).sum()
    }

    /// 下载落盘的位置：多文件种子会在输出目录下建一层以种子名命名的目录
    fn output_dir(&self, download_dir: &std::path::Path) -> PathBuf {
        download_dir.join(&self.folder)
    }
}

/// 把种子挂到做种会话上。
/// `upload_bps` 给做种端限速：回环直连快到毫秒级，不限速就没机会在中途暂停或改勾选。
async fn seed_torrent(
    root: &TempDir,
    torrent: &CreateTorrentResult,
    source: PathBuf,
    upload_bps: Option<u32>,
) -> Arc<BtRuntime> {
    let seeding = BtRuntime::local(root.join("seed-state"), Vec::new())
        .await
        .expect("启动做种会话失败");
    let handle = seeding
        .session()
        .add_torrent(
            AddTorrent::from_bytes(torrent.as_bytes().expect("序列化种子失败")),
            Some(AddTorrentOptions {
                output_folder: Some(source.to_string_lossy().to_string()),
                overwrite: true,
                ratelimits: LimitsConfig {
                    upload_bps: upload_bps.and_then(NonZeroU32::new),
                    download_bps: None,
                },
                ..Default::default()
            }),
        )
        .await
        .expect("做种会话挂载种子失败")
        .into_handle()
        .expect("做种会话没有返回句柄");
    handle.wait_until_initialized().await.expect("做种初始化失败");
    seeding
}

/// 单文件夹具：一个文件、一个种子。
async fn setup(upload_bps: Option<u32>) -> Harness {
    let root = TempDir::new("bt-fixture");
    let source_dir = root.join("source");
    std::fs::create_dir_all(&source_dir).expect("创建源目录失败");

    // 用确定性的伪随机内容，比对时能逐字节核对
    let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| (i * 31 % 251) as u8).collect();
    let source_file = source_dir.join("payload.bin");
    std::fs::write(&source_file, &payload).expect("写入源文件失败");

    let torrent = create_torrent(
        &source_file,
        CreateTorrentOptions {
            name: Some("payload.bin"),
            trackers: Vec::new(),
            piece_length: Some(64 * 1024),
        },
        &BlockingSpawner::new(1),
    )
    .await
    .expect("生成种子失败");

    let seeding = seed_torrent(&root, &torrent, source_dir, upload_bps).await;

    Harness {
        root,
        payload,
        torrent,
        _seeding: seeding,
    }
}

/// 多文件夹具。文件大小各不相同，方便断言「只下了选中的那个」。
/// alpha 特意做得比 librqbit 的请求窗口（128 个分片 ≈ 2 MiB）大得多：
/// 收窄勾选时已经在途的分片会照常写完，只有远超窗口的文件才能确定「没被下完」。
async fn setup_multi(upload_bps: Option<u32>) -> MultiHarness {
    const FILES: [(&str, usize); 3] = [
        ("alpha.bin", 24 * 1024 * 1024),
        ("beta.bin", 1024 * 1024 + 777),
        ("gamma.bin", 512 * 1024),
    ];

    let root = TempDir::new("bt-multi");
    let folder = "payload-dir".to_string();
    let source_dir = root.join(&folder);
    std::fs::create_dir_all(&source_dir).expect("创建源目录失败");

    let mut contents = Vec::new();
    for (index, (name, len)) in FILES.iter().enumerate() {
        // 内容里不出现 0 字节，于是「文件里全零」可以当作「这块没下过」的证据
        let data: Vec<u8> = (0..*len)
            .map(|i| (((i + index * 7) * 17) % 251) as u8 + 1)
            .collect();
        std::fs::write(source_dir.join(name), &data).expect("写入源文件失败");
        contents.push((name.to_string(), data));
    }

    let torrent = create_torrent(
        &source_dir,
        CreateTorrentOptions {
            name: None,
            trackers: Vec::new(),
            piece_length: Some(64 * 1024),
        },
        &BlockingSpawner::new(1),
    )
    .await
    .expect("生成多文件种子失败");

    let seeding = seed_torrent(&root, &torrent, source_dir, upload_bps).await;

    MultiHarness {
        root,
        contents,
        folder,
        torrent,
        _seeding: seeding,
    }
}

/// 做种者的监听地址：会话在后台绑定端口，绑好之前拿到的是 None。
async fn seeding_addr(seeding: &BtRuntime) -> std::net::SocketAddr {
    for _ in 0..100 {
        if let Some(addr) = seeding.listen_addr() {
            return addr;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("做种会话没有在 2 秒内开始监听");
}

fn collecting_sink() -> (EventSink, Arc<Mutex<Vec<TaskView>>>) {
    let views = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&views);
    let sink: EventSink = Arc::new(move |event| {
        if let ManagerEvent::Update(view) = event {
            collected.lock().unwrap().push(view);
        }
    });
    (sink, views)
}

async fn wait_until<F: FnMut() -> bool>(timeout: Duration, what: &str, mut cond: F) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等待「{what}」超时");
}

fn view_of(manager: &DownloadManager, id: &str) -> Option<TaskView> {
    manager.list().into_iter().find(|view| view.id == id)
}

/// 下载端：一个管理器 + 一个只连做种者的会话，目录按 tag 分开。
async fn start_client(
    root: &TempDir,
    store: Arc<Store>,
    peer: std::net::SocketAddr,
    tag: &str,
) -> (Arc<DownloadManager>, PathBuf) {
    let dir = root.join(format!("downloads-{tag}"));
    std::fs::create_dir_all(&dir).expect("创建下载目录失败");
    let (sink, _) = collecting_sink();
    let manager = DownloadManager::new(sink, dir.clone(), Some(store));
    let runtime = BtRuntime::local(root.join(format!("state-{tag}")), vec![peer])
        .await
        .expect("启动下载会话失败");
    manager
        .install_bt_runtime(runtime)
        .expect("装载 BT 会话失败");
    (manager, dir)
}

fn magnet_of(harness: &Harness) -> String {
    harness.torrent.as_magnet().to_string()
}

fn add_bt_task(
    manager: &Arc<DownloadManager>,
    magnet: String,
    dir: &PathBuf,
    only_files: Option<Vec<usize>>,
) -> String {
    manager
        .add(
            magnet,
            Some(dir.to_string_lossy().to_string()),
            None,
            only_files,
        )
        .expect("添加磁力任务失败")
}

/// 7a 的完成标志：磁力能拿到元数据、文件按种子里的名字落盘、
/// info hash 与磁力都写进数据库，且整份内容与源文件逐字节一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magnet_task_fetches_metadata_and_downloads() {
    let harness = setup(None).await;
    let seed_addr = seeding_addr(&harness._seeding).await;
    let magnet = harness.torrent.as_magnet().to_string();
    let expected_hash = harness.torrent.info_hash().as_string();

    let download_dir = harness.root.join("downloads");
    std::fs::create_dir_all(&download_dir).unwrap();
    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (sink, views) = collecting_sink();
    let manager = DownloadManager::new(sink, download_dir.clone(), Some(Arc::clone(&store)));
    let downloading = BtRuntime::local(harness.root.join("dl-state"), vec![seed_addr])
        .await
        .expect("启动下载会话失败");
    manager
        .install_bt_runtime(downloading)
        .expect("装载 BT 会话失败");

    let id = manager
        .add(
            magnet.clone(),
            Some(download_dir.to_string_lossy().to_string()),
            None,
            None,
        )
        .expect("添加磁力任务失败");

    // 入队即应有任务，且此刻还不知道总大小（界面据此显示「解析元数据」）
    let initial = view_of(&manager, &id).expect("任务应已入队");
    assert_eq!(initial.kind, TaskKind::Bt);
    assert_eq!(initial.total_bytes, 0, "元数据到手前不应知道总大小");

    wait_until(Duration::from_secs(90), "下载完成", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let done = view_of(&manager, &id).expect("任务应还在");
    assert_eq!(done.total_bytes, PAYLOAD_LEN as u64);
    assert_eq!(done.downloaded_bytes, PAYLOAD_LEN as u64);
    assert_eq!(done.file_name, "payload.bin");
    assert_eq!(
        PathBuf::from(&done.save_path),
        download_dir.join("payload.bin")
    );

    // 界面先看到过「还不知道大小」的阶段，再看到进度
    let seen = views.lock().unwrap();
    assert!(
        seen.iter().any(|view| view.total_bytes == 0),
        "应至少有一帧处于解析元数据阶段"
    );
    assert!(
        seen.iter()
            .any(|view| view.total_bytes == PAYLOAD_LEN as u64 && view.downloaded_bytes > 0),
        "应至少有一帧带着下载进度"
    );
    drop(seen);

    let downloaded = std::fs::read(download_dir.join("payload.bin")).expect("下载的文件不在");
    assert_eq!(downloaded.len(), harness.payload.len());
    assert!(downloaded == harness.payload, "下载内容与源文件不一致");

    // 落库：协议、info hash 与磁力原文都要在，跨会话恢复靠它们
    let record = store
        .load_tasks()
        .expect("读取任务失败")
        .into_iter()
        .find(|record| record.id == id)
        .expect("任务应已落库");
    assert_eq!(record.kind, "bt");
    assert_eq!(record.info_hash.as_deref(), Some(expected_hash.as_str()));
    assert_eq!(record.magnet.as_deref(), Some(magnet.as_str()));
    assert_eq!(record.file_name, "payload.bin");
    assert_eq!(record.total_bytes, PAYLOAD_LEN as i64);
}

/// 7b 的完成标志：限速传输途中暂停，进度原样保留，继续后整份文件逐字节正确。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_and_resume_keep_progress_and_finish_file() {
    let harness = setup(Some(512 * 1024)).await;
    let peer = seeding_addr(&harness._seeding).await;
    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "pause").await;
    let id = add_bt_task(&manager, magnet_of(&harness), &dir, None);

    // peer 统计：连上做种者后界面上应该有「已连 1 个」
    wait_until(Duration::from_secs(30), "连上做种者", || {
        view_of(&manager, &id)
            .and_then(|view| view.bt_stats)
            .is_some_and(|stats| stats.peers_connected >= 1 && stats.peers_seen >= 1)
    })
    .await;

    wait_until(Duration::from_secs(30), "出现下载进度", || {
        view_of(&manager, &id).is_some_and(|view| view.downloaded_bytes > 0)
    })
    .await;

    manager.pause(&id).expect("暂停应成功");
    wait_until(Duration::from_secs(20), "暂停收尾", || {
        !manager.is_running(&id)
            && view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Paused)
    })
    .await;

    let paused = view_of(&manager, &id).expect("任务应还在");
    assert!(paused.downloaded_bytes > 0, "暂停应保留已下载的进度");
    assert!(
        paused.downloaded_bytes < PAYLOAD_LEN as u64,
        "限速下不该已经下完，实际 {}",
        paused.downloaded_bytes
    );

    // 暂停状态与断点进度都要落库，重启后才恢复得出「已暂停、下到一半」
    let record = store
        .load_tasks()
        .expect("读取任务失败")
        .into_iter()
        .find(|record| record.id == id)
        .expect("任务应已落库");
    assert_eq!(record.status, "paused");
    assert_eq!(record.total_bytes, PAYLOAD_LEN as i64);
    assert!(
        record.error.is_none(),
        "暂停不该被记成失败：{:?}",
        record.error
    );

    manager.resume(&id).expect("继续应成功");
    wait_until(Duration::from_secs(90), "继续后下载完成", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let done = view_of(&manager, &id).expect("任务应还在");
    assert_eq!(done.downloaded_bytes, PAYLOAD_LEN as u64);
    let written = std::fs::read(&done.save_path).expect("下载的文件不在");
    assert!(
        written == harness.payload,
        "暂停继续之后的文件必须与源文件逐字节一致"
    );
}

/// 7c 的完成标志：关掉应用（丢掉管理器与会话）后重新打开，
/// 任务带着断点进度恢复成已暂停，点继续能接着下完。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bt_task_survives_restart_and_resumes() {
    let harness = setup(Some(512 * 1024)).await;
    let peer = seeding_addr(&harness._seeding).await;
    let db_path = harness.root.join("conflux.db");
    let dir = harness.root.join("downloads-restart");
    std::fs::create_dir_all(&dir).unwrap();
    let magnet = magnet_of(&harness);

    // 第一次运行：下出一段进度后暂停并退出
    let id = {
        let store = Arc::new(Store::open(&db_path).expect("打开数据库失败"));
        let (sink, _) = collecting_sink();
        let manager = DownloadManager::new(sink, dir.clone(), Some(store));
        let runtime = BtRuntime::local(harness.root.join("state-first"), vec![peer])
            .await
            .expect("启动下载会话失败");
        manager.install_bt_runtime(runtime).unwrap();

        let id = add_bt_task(&manager, magnet.clone(), &dir, None);
        wait_until(Duration::from_secs(30), "出现下载进度", || {
            view_of(&manager, &id).is_some_and(|view| view.downloaded_bytes > 0)
        })
        .await;
        manager.pause(&id).expect("暂停应成功");
        wait_until(Duration::from_secs(20), "暂停收尾", || !manager.is_running(&id)).await;
        id
    };

    {
        let probe = Store::open(&db_path).expect("探测读取失败");
        let rows = probe.load_tasks().expect("读取任务失败");
        println!(
            "第一次运行结束后库里 {} 行：{:?}",
            rows.len(),
            rows.iter().map(|r| (&r.id, &r.status)).collect::<Vec<_>>()
        );
        assert_eq!(rows.len(), 1, "第一次运行就该把任务落库");
    }

    // 第二次运行：重新打开数据库与新的会话
    let store = Arc::new(Store::open(&db_path).expect("重新打开数据库失败"));
    let (manager, _) = start_client(&harness.root, Arc::clone(&store), peer, "second").await;
    let restored = manager.list();
    assert_eq!(restored.len(), 1, "重启后应恢复出 1 个任务");
    let task = &restored[0];
    assert_eq!(task.id, id);
    assert_eq!(task.kind, TaskKind::Bt);
    assert_eq!(task.status, TaskStatus::Paused);
    assert_eq!(task.file_name, "payload.bin");
    assert_eq!(task.total_bytes, PAYLOAD_LEN as u64);
    assert!(
        task.downloaded_bytes > 0 && task.downloaded_bytes < PAYLOAD_LEN as u64,
        "重启后应带着断点进度，实际 {}",
        task.downloaded_bytes
    );

    manager.resume(&id).expect("继续应成功");
    wait_until(Duration::from_secs(90), "继续后下载完成", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let done = view_of(&manager, &id).expect("任务应还在");
    let written = std::fs::read(&done.save_path).expect("下载的文件不在");
    assert!(
        written == harness.payload,
        "跨会话续传后的文件必须与源文件逐字节一致"
    );
}

/// 种子文件入口：元数据随文件自带，入队就该知道名字与总大小，不必等 peer；
/// 下载结果与磁力入口完全一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torrent_file_task_knows_size_up_front_and_downloads() {
    let harness = setup(Some(512 * 1024)).await;
    let peer = seeding_addr(&harness._seeding).await;
    let torrent_path = harness.root.join("payload.torrent");
    std::fs::write(
        &torrent_path,
        harness.torrent.as_bytes().expect("序列化种子失败"),
    )
    .expect("写入种子文件失败");

    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "torrent-file").await;
    let id = manager
        .add_torrent_file(
            torrent_path.to_string_lossy().to_string(),
            Some(dir.to_string_lossy().to_string()),
            None,
            None,
        )
        .expect("添加种子文件任务失败");

    let queued = view_of(&manager, &id).expect("任务应已入队");
    assert_eq!(queued.kind, TaskKind::Bt);
    assert_eq!(queued.file_name, "payload.bin");
    assert_eq!(
        queued.total_bytes, PAYLOAD_LEN as u64,
        "种子文件自带元数据，入队时就该知道总大小"
    );
    assert_eq!(queued.info_hash.as_deref(), Some(harness.torrent.info_hash().as_string().as_str()));

    wait_until(Duration::from_secs(90), "下载完成", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let done = view_of(&manager, &id).expect("任务应还在");
    let written = std::fs::read(&done.save_path).expect("下载的文件不在");
    assert!(written == harness.payload, "种子文件下载的内容必须与源文件一致");

    // 落库记的是种子文件路径，磁力为空；重启后靠它重新挂载
    let record = store
        .load_tasks()
        .expect("读取任务失败")
        .into_iter()
        .find(|record| record.id == id)
        .expect("任务应已落库");
    assert_eq!(record.kind, "bt");
    assert!(record.magnet.is_none(), "种子文件任务没有磁力");
    assert_eq!(
        record.torrent_path.as_deref(),
        Some(torrent_path.to_string_lossy().as_ref())
    );
}

/// 把 .torrent 路径粘进「链接」输入框也要按种子文件处理，而不是报「不支持的协议」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pasting_a_torrent_path_is_treated_as_a_torrent_task() {
    let harness = setup(None).await;
    let torrent_path = harness.root.join("pasted.torrent");
    std::fs::write(
        &torrent_path,
        harness.torrent.as_bytes().expect("序列化种子失败"),
    )
    .expect("写入种子文件失败");

    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let dir = harness.root.join("downloads-pasted");
    let (sink, _) = collecting_sink();
    let manager = DownloadManager::new(sink, dir.clone(), Some(store));
    manager
        .install_bt_runtime(
            BtRuntime::local(harness.root.join("state-pasted"), Vec::new())
                .await
                .expect("启动会话失败"),
        )
        .unwrap();

    let id = manager
        .add(
            format!("  {}  ", torrent_path.to_string_lossy()),
            Some(dir.to_string_lossy().to_string()),
            None,
            None,
        )
        .expect("粘贴种子文件路径应能建任务");
    let view = view_of(&manager, &id).expect("任务应已入队");
    assert_eq!(view.kind, TaskKind::Bt);
    assert_eq!(view.file_name, "payload.bin");
    assert_eq!(view.total_bytes, PAYLOAD_LEN as u64);

    manager.remove(&id, false).expect("移除应成功");
}

/// 种子文件入口 + 只下勾选的：多文件种子的内容落在以种子名命名的子目录里，
/// 勾选在入队时就能生效（清单随文件自带，不必等 peer）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn torrent_file_with_selection_lands_in_name_folder() {
    let harness = setup_multi(None).await;
    let peer = seeding_addr(&harness._seeding).await;
    let torrent_path = harness.root.join("multi.torrent");
    std::fs::write(
        &torrent_path,
        harness.torrent.as_bytes().expect("序列化种子失败"),
    )
    .expect("写入种子文件失败");

    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "torrent-select").await;
    let beta = manager
        .preview_torrent_file(torrent_path.to_string_lossy().to_string())
        .expect("预览种子文件应成功")
        .files
        .into_iter()
        .find(|file| file.name == "beta.bin")
        .expect("预览里应有 beta.bin");

    let id = manager
        .add_torrent_file(
            torrent_path.to_string_lossy().to_string(),
            Some(dir.to_string_lossy().to_string()),
            None,
            Some(vec![beta.index]),
        )
        .expect("添加种子文件任务失败");

    // 入队即知清单与勾选，不必等元数据
    let queued = view_of(&manager, &id).expect("任务应已入队");
    assert_eq!(queued.file_name, harness.folder, "多文件种子的名字是目录名");
    assert_eq!(
        PathBuf::from(&queued.save_path),
        harness.output_dir(&dir),
        "多文件种子的内容在任务目录下再套一层以种子名命名的目录"
    );
    assert_eq!(queued.files.len(), 3);
    assert_eq!(
        queued.files.iter().filter(|file| file.selected).count(),
        1,
        "入队时就该按勾选标记"
    );

    wait_until(Duration::from_secs(60), "下载完成", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let output = harness.output_dir(&dir);
    assert!(
        std::fs::read(output.join("beta.bin")).expect("beta.bin 应已落盘")
            == harness.content("beta.bin"),
        "勾选的文件内容必须与源文件一致"
    );
    let alpha_bytes = std::fs::read(output.join("alpha.bin")).unwrap_or_default();
    assert!(
        alpha_bytes.iter().all(|byte| *byte == 0),
        "没勾选的文件不该有内容"
    );
}

/// 移除带删文件：数据由会话按种子记录的文件清单清理，任务行也从库里消失。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_running_task_deletes_downloaded_files() {
    let harness = setup(Some(512 * 1024)).await;
    let peer = seeding_addr(&harness._seeding).await;
    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "remove").await;
    let id = add_bt_task(&manager, magnet_of(&harness), &dir, None);

    wait_until(Duration::from_secs(30), "出现下载进度", || {
        view_of(&manager, &id).is_some_and(|view| view.downloaded_bytes > 0)
    })
    .await;

    manager.remove(&id, true).expect("移除应成功");
    let path = dir.join("payload.bin");
    wait_until(Duration::from_secs(20), "任务与文件一起消失", || {
        view_of(&manager, &id).is_none() && !path.exists()
    })
    .await;
    assert!(
        store
            .load_tasks()
            .expect("读取任务失败")
            .iter()
            .all(|record| record.id != id),
        "移除后数据库不该再有这个任务"
    );
}

/// 预览的完成标志：只取元数据看一眼文件清单，不建任务、不落库、不落盘；
/// 用户看完再决定勾哪几个，全程不联网到外网（只连夹具里的做种者）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preview_lists_files_without_downloading() {
    let harness = setup_multi(None).await;
    let peer = seeding_addr(&harness._seeding).await;
    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "preview").await;

    let preview = manager
        .preview_magnet(harness.torrent.as_magnet().to_string())
        .await
        .expect("预览应成功");

    assert_eq!(preview.info_hash, harness.torrent.info_hash().as_string());
    assert_eq!(preview.name.as_deref(), Some(harness.folder.as_str()));
    assert_eq!(preview.total_bytes, harness.total_bytes());

    let mut names: Vec<&str> = preview.files.iter().map(|file| file.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["alpha.bin", "beta.bin", "gamma.bin"]);
    for file in &preview.files {
        assert_eq!(
            file.length,
            harness.content(&file.name).len() as u64,
            "{} 的长度应与源文件一致",
            file.name
        );
        assert!(file.selected, "预览默认全选");
    }

    // 预览是「只看一眼」：不建任务、不落库、也不在任何地方写出文件
    assert!(manager.list().is_empty(), "预览不该建任务");
    assert!(
        store.load_tasks().expect("读取任务失败").is_empty(),
        "预览不该落库"
    );
    assert!(
        std::fs::read_dir(&dir).expect("下载目录应在").next().is_none(),
        "预览不该在下载目录里留下任何东西"
    );
}

/// 选择性下载的完成标志：勾一个文件，只有它被完整下载，其余的最多被边界分片蹭到一点。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selecting_files_downloads_only_those() {
    let harness = setup_multi(None).await;
    let peer = seeding_addr(&harness._seeding).await;
    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "select").await;

    // 界面上就是这条流程：先预览拿到清单与序号，再按序号建任务
    let preview = manager
        .preview_magnet(harness.torrent.as_magnet().to_string())
        .await
        .expect("预览应成功");
    let beta = preview
        .files
        .iter()
        .find(|file| file.name == "beta.bin")
        .expect("预览里应有 beta.bin")
        .clone();

    let id = add_bt_task(
        &manager,
        harness.torrent.as_magnet().to_string(),
        &dir,
        Some(vec![beta.index]),
    );

    wait_until(Duration::from_secs(60), "只下勾选的文件", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let done = view_of(&manager, &id).expect("任务应还在");
    // 总量按「勾选的文件所覆盖的分片」算，边界分片会多带一点相邻文件的字节
    let piece = 64 * 1024u64;
    assert!(
        done.total_bytes >= beta.length && done.total_bytes <= beta.length + piece,
        "总量应贴近 beta.bin 的大小，实际 {}",
        done.total_bytes
    );
    assert!(
        done.total_bytes < harness.total_bytes(),
        "只勾了一个文件，总量不该是整个种子的大小"
    );
    assert_eq!(done.downloaded_bytes, done.total_bytes, "完成后进度应满");
    assert_eq!(done.files.len(), 3, "元数据到手后应能看到完整清单");
    for file in &done.files {
        assert_eq!(
            file.selected,
            file.name == "beta.bin",
            "{} 的勾选状态不对",
            file.name
        );
    }
    let beta_view = done.files.iter().find(|file| file.name == "beta.bin").unwrap();
    assert_eq!(beta_view.downloaded, beta.length, "勾选的文件应下满");
    // 没勾的文件不该被下载：与它无关的分片根本不会进队列
    let alpha_view = done.files.iter().find(|file| file.name == "alpha.bin").unwrap();
    assert_eq!(alpha_view.downloaded, 0, "alpha.bin 没勾选，不该有已下载字节");
    let gamma_view = done.files.iter().find(|file| file.name == "gamma.bin").unwrap();
    assert!(
        gamma_view.downloaded < gamma_view.length,
        "gamma.bin 没勾选，不该下完，实际 {}",
        gamma_view.downloaded
    );

    // 落盘：勾中的文件逐字节正确；没勾的只有空占位，
    // 唯一的例外是与勾选文件共用边界分片的那个，它的开头会被分片带上几十 KB
    // （夹具内容不含 0，全零即「没下过」）
    let output = harness.output_dir(&dir);
    assert!(
        std::fs::read(output.join("beta.bin")).expect("beta.bin 应已落盘")
            == harness.content("beta.bin"),
        "勾选的文件内容必须与源文件一致"
    );
    let alpha_bytes = std::fs::read(output.join("alpha.bin")).unwrap_or_default();
    assert!(
        alpha_bytes.iter().all(|byte| *byte == 0),
        "alpha.bin 没勾选，不该有内容，实际 {} 字节",
        alpha_bytes.len()
    );
    let before_gamma =
        (harness.content("alpha.bin").len() + harness.content("beta.bin").len()) as u64;
    let boundary_in_gamma = (piece - before_gamma % piece) % piece;
    let gamma_bytes = std::fs::read(output.join("gamma.bin")).unwrap_or_default();
    assert!(
        gamma_bytes[boundary_in_gamma as usize..]
            .iter()
            .all(|byte| *byte == 0),
        "gamma.bin 没勾选，分片边界之后不该有内容"
    );

    // 勾选要落库，否则重启后会把没勾的文件也下下来
    let record = store
        .load_tasks()
        .expect("读取任务失败")
        .into_iter()
        .find(|record| record.id == id)
        .expect("任务应已落库");
    assert_eq!(
        record.only_files.as_deref(),
        Some(format!("[{}]", beta.index).as_str()),
        "勾选序号应写进数据库"
    );

    // 一个都不选没有意义：界面上最后一个勾选着的文件是锁住的，接口层也要拦住
    assert!(manager.set_files(&id, vec![]).is_err(), "空勾选应被拒绝");
}

/// 下载途中改勾选：把没勾的文件撤下来，任务应当在剩下的文件下完后正常结束。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn narrowing_selection_midflight_still_finishes() {
    let harness = setup_multi(Some(512 * 1024)).await;
    let peer = seeding_addr(&harness._seeding).await;
    let store = Arc::new(Store::open_in_memory().expect("打开数据库失败"));
    let (manager, dir) = start_client(&harness.root, Arc::clone(&store), peer, "narrow").await;

    let preview = manager
        .preview_magnet(harness.torrent.as_magnet().to_string())
        .await
        .expect("预览应成功");
    let beta = preview
        .files
        .iter()
        .find(|file| file.name == "beta.bin")
        .expect("预览里应有 beta.bin")
        .clone();

    // 先全选开跑，下起来之后再收窄
    let id = add_bt_task(
        &manager,
        harness.torrent.as_magnet().to_string(),
        &dir,
        None,
    );
    wait_until(Duration::from_secs(30), "出现下载进度", || {
        view_of(&manager, &id).is_some_and(|view| view.downloaded_bytes > 0)
    })
    .await;

    manager
        .set_files(&id, vec![beta.index])
        .expect("改勾选应成功");

    // 勾选是被会话在下一拍采纳的：总量随之只算勾选覆盖的分片
    wait_until(Duration::from_secs(30), "收窄生效", || {
        view_of(&manager, &id).is_some_and(|view| view.total_bytes < harness.total_bytes())
    })
    .await;

    let narrowed = view_of(&manager, &id).expect("任务应还在");
    assert!(
        narrowed.files
            .iter()
            .find(|file| file.name == "beta.bin")
            .is_some_and(|file| file.selected),
        "beta.bin 应保持勾选"
    );

    wait_until(Duration::from_secs(90), "收窄后下载完成", || {
        view_of(&manager, &id).map(|view| view.status) == Some(TaskStatus::Completed)
    })
    .await;

    let output = harness.output_dir(&dir);
    assert!(
        std::fs::read(output.join("beta.bin")).expect("beta.bin 应已落盘")
            == harness.content("beta.bin"),
        "收窄后勾选的文件必须下完且内容一致"
    );

    // 收窄时已经下过的片段留在磁盘上（不清零也不删），但没勾的文件不该下完
    let done = view_of(&manager, &id).expect("任务应还在");
    let alpha_view = done.files.iter().find(|file| file.name == "alpha.bin").unwrap();
    assert!(!alpha_view.selected, "alpha.bin 应已取消勾选");
    assert!(
        alpha_view.downloaded < alpha_view.length,
        "取消勾选后 alpha.bin 不该下完，实际 {}/{}",
        alpha_view.downloaded,
        alpha_view.length
    );
}
