use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;

/// 按绝对偏移写入，多个通道共享同一个文件句柄而不需要全局 seek 锁。
#[cfg(unix)]
pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

#[cfg(windows)]
pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut written = 0usize;
    while written < buf.len() {
        let n = file.seek_write(&buf[written..], offset + written as u64)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "写入返回 0 字节"));
        }
        written += n;
    }
    Ok(())
}

/// 去掉路径分隔符与控制字符，避免服务端提供的名称写到目录之外。
pub fn sanitize_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push('_'),
            c => out.push(c),
        }
    }
    out.trim_matches(|c: char| c == ' ' || c == '.').to_string()
}

pub fn file_name_from_url(url: &str) -> Option<String> {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment.split('?').next().unwrap_or(without_fragment);
    let last = without_query.trim_end_matches('/').rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    let decoded = percent_decode_str(last).decode_utf8_lossy().to_string();
    let cleaned = sanitize_file_name(&decoded);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// 优先解析 RFC 5987 的 filename*，其次回退到 filename。
pub fn file_name_from_disposition(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename*=") {
            let rest = rest.trim_matches('"');
            let raw = rest.splitn(3, '\'').nth(2).unwrap_or(rest);
            let decoded = percent_decode_str(raw).decode_utf8_lossy().to_string();
            let cleaned = sanitize_file_name(&decoded);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            let rest = rest.trim_matches('"');
            let cleaned = sanitize_file_name(rest);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

pub fn part_path(save_path: &Path) -> PathBuf {
    let mut name = save_path.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = path.extension().map(|s| s.to_string_lossy().to_string());
    for i in 1..10_000u32 {
        let file_name = match &ext {
            Some(ext) => format!("{stem} ({i}).{ext}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = dir.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}
