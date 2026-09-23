use std::path::Path;
use std::time::{Duration, Instant};

use conflux_lib::download::bt::{
    normalize_proxy, parse_magnet, parse_torrent_file, preview_magnet, BtRuntime,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let target = args
        .next()
        .ok_or("用法：bt_probe <磁力或 .torrent 路径> [等待秒数]")?;
    let wait = args
        .next()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(90);
    if wait == 0 || args.next().is_some() {
        return Err("用法：bt_probe <磁力或 .torrent 路径> [正整数等待秒数]".into());
    }

    let level = std::env::var("RUST_LOG")
        .ok()
        .map(|value| value.parse::<tracing_subscriber::filter::LevelFilter>())
        .transpose()?
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();

    let started = Instant::now();
    // 只给探针用的开关：把 BT 会话接到 SOCKS5 代理上（产品的代理在设置界面里配）
    let proxy = match std::env::var("CONFLUX_BT_PROXY") {
        Ok(raw) => normalize_proxy(&raw)?,
        Err(_) => None,
    };
    let preview = if target.starts_with("magnet:") {
        parse_magnet(&target)?;
        let runtime = BtRuntime::new_with_proxy(std::env::temp_dir(), proxy.clone()).await?;
        println!("复用产品会话，只取元数据，不下载内容；未更改系统与环境代理设置");
        match &proxy {
            Some(proxy) => println!("BT 走 SOCKS5 代理：{proxy}（DHT 已关，peer 与 tracker 经代理）"),
            None => println!("未用代理（可用 CONFLUX_BT_PROXY=socks5://… 指定）"),
        }
        println!("监听地址：{:?}", runtime.listen_addr());
        let resolving = preview_magnet(&runtime, &target, Duration::from_secs(wait));
        tokio::pin!(resolving);
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                result = &mut resolving => break result?,
                _ = tick.tick() => {
                    if let Some(dht) = runtime.session().get_dht() {
                        let stats = dht.stats();
                        println!(
                            "  {} 秒，DHT 路由节点 IPv4={} / IPv6={}，未决请求={}（不是 peer 数）",
                            started.elapsed().as_secs(),
                            stats.routing_table_size,
                            stats.routing_table_size_v6,
                            stats.outstanding_requests
                        );
                    }
                }
            }
        }
    } else {
        println!("仅读取本地种子元数据，不验证网络或 peer 连通性");
        parse_torrent_file(Path::new(&target))?
    };

    println!(
        "{:.2} 秒后取得元数据：info hash={}，大小={}，文件数={}",
        started.elapsed().as_secs_f64(),
        preview.info_hash,
        preview.total_bytes,
        preview.files.len()
    );
    Ok(())
}
