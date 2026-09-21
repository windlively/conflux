//! 手动联调用的做种端：把本地文件做成种子挂到一个 tracker 上做种，并打印磁力。
//!
//! 生产会话靠 DHT／tracker 找 peer，因此本机联调需要一个能被找到的做种者：
//! 起一个极简本地 tracker，把它的地址交给这个示例，磁力里带上同一个 tracker，
//! 应用就能在完全离线的情况下走完真实的磁力下载链路。
//!
//! 用法：
//!   cargo run --example bt_seed -- <文件或目录> <tracker 地址> [上传限速 KB/s] [种子文件输出路径]
//! 例如：
//!   cargo run --example bt_seed -- /tmp/payload.bin http://127.0.0.1:8790/announce 2048 /tmp/payload.torrent
//!   cargo run --example bt_seed -- /tmp/payload-dir http://127.0.0.1:8790/announce 2048 /tmp/payload.torrent
//! 给第四个参数时顺带写出一份 .torrent，用来验证「导入种子文件」这条入口。

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use conflux_lib::download::bt::BtRuntime;
use librqbit::limits::LimitsConfig;
use librqbit::spawn_utils::BlockingSpawner;
use librqbit::{create_torrent, AddTorrent, AddTorrentOptions, CreateTorrentOptions};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("用法：bt_seed <文件> <tracker 地址> [限速 KB/s]"));
    let tracker = args.next().expect("缺少 tracker 地址");
    let upload_kbps: Option<u32> = args.next().map(|value| value.parse().expect("限速要是个数字"));
    let torrent_out: Option<PathBuf> = args.next().map(PathBuf::from);

    let torrent = create_torrent(
        &path,
        CreateTorrentOptions {
            name: None,
            trackers: vec![tracker],
            piece_length: Some(256 * 1024),
        },
        &BlockingSpawner::new(1),
    )
    .await
    .expect("生成种子失败");

    // 目录直接做种就是多文件种子；单文件种子的输出目录取它所在的目录
    let dir = if path.is_dir() {
        path.clone()
    } else {
        path.parent().expect("文件没有所在目录").to_path_buf()
    };
    // 复用应用的会话配置：它显式打开了监听（librqbit 默认配置里 listen 是 None，
    // 不显式打开就根本不起监听，tracker 上登记出去的端口会是假的）
    let runtime = BtRuntime::new(dir.clone())
        .await
        .expect("启动做种会话失败");
    let session = runtime.session();
    // 等监听就绪再挂种子：tracker 上登记的端口取自会话的监听端口
    for _ in 0..200 {
        if runtime.listen_addr().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let handle = session
        .add_torrent(
            AddTorrent::from_bytes(torrent.as_bytes().expect("序列化种子失败")),
            Some(AddTorrentOptions {
                output_folder: Some(dir.to_string_lossy().to_string()),
                overwrite: true,
                ratelimits: LimitsConfig {
                    upload_bps: upload_kbps.and_then(|kbps| NonZeroU32::new(kbps * 1024)),
                    download_bps: None,
                },
                ..Default::default()
            }),
        )
        .await
        .expect("挂载种子失败")
        .into_handle()
        .expect("会话没有返回句柄");
    handle.wait_until_initialized().await.expect("做种初始化失败");

    if let Some(out) = torrent_out {
        std::fs::write(&out, torrent.as_bytes().expect("序列化种子失败")).expect("写出种子文件失败");
        println!("种子文件：{}", out.display());
    }
    println!("磁力：{}", torrent.as_magnet());
    println!("info hash：{}", torrent.info_hash().as_string());
    println!("监听地址：{:?}", runtime.listen_addr());
    println!("做种中，Ctrl-C 退出");

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
