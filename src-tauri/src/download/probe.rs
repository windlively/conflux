use std::time::Duration;

use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, RANGE,
};
use reqwest::{Client, StatusCode};

use super::path::file_name_from_disposition;

pub struct ProbeResult {
    pub total: Option<u64>,
    pub supports_range: bool,
    pub file_name: Option<String>,
}

fn header_string(headers: &reqwest::header::HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// 只依赖 HEAD 或 Accept-Ranges 判断分段能力并不可靠，
/// 因此这里再用 `Range: bytes=0-0` 实际验证服务器是否返回 206。
pub async fn probe(client: &Client, url: &str) -> Result<ProbeResult, String> {
    let mut total: Option<u64> = None;
    let mut file_name = None;

    if let Ok(resp) = client.head(url).send().await {
        if resp.status().is_success() {
            let headers = resp.headers();
            total = header_string(headers, CONTENT_LENGTH).and_then(|v| v.parse().ok());
            file_name = header_string(headers, CONTENT_DISPOSITION)
                .and_then(|v| file_name_from_disposition(&v));
        }
    }

    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .header(ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|e| format!("无法访问资源：{e}"))?;

    let status = resp.status();
    let headers = resp.headers().to_owned();

    let supports_range = if status == StatusCode::PARTIAL_CONTENT {
        if let Some(raw) = header_string(&headers, CONTENT_RANGE) {
            if let Some(total_part) = raw.rsplit('/').next() {
                if let Ok(parsed) = total_part.trim().parse::<u64>() {
                    total = Some(parsed);
                }
            }
        }
        true
    } else if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // 空文件：服务器拒绝 0-0 区间
        total = Some(0);
        false
    } else if status.is_success() {
        // 服务器忽略 Range 返回 200：即使它声明 Accept-Ranges 也不能按分段处理
        if total.is_none() {
            total = header_string(&headers, CONTENT_LENGTH).and_then(|v| v.parse().ok());
        }
        false
    } else {
        return Err(format!("服务器返回 HTTP {}", status.as_u16()));
    };

    if file_name.is_none() {
        file_name = header_string(&headers, CONTENT_DISPOSITION)
            .and_then(|v| file_name_from_disposition(&v));
    }

    // 释放响应体，避免把整个文件拉下来
    drop(resp);

    Ok(ProbeResult {
        total,
        supports_range,
        file_name,
    })
}

pub fn client() -> Client {
    Client::builder()
        .user_agent(concat!("conflux/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        // 分段下载依赖多条独立 TCP 连接：HTTP/2 会把并发分片复用回单条连接，
        // 单连接拥塞窗口反而成为瓶颈，因此这里固定 HTTP/1.1。
        .http1_only()
        .build()
        .expect("构建 HTTP 客户端失败")
}
