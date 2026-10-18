//! 解決結果のキャッシュ。
//! ワールドの動画プレイヤーは読み込みに時間がかかると同じ URL を即リロードしてくるので、
//! 直前の結果を返して yt-dlp の再実行(数秒)を省く。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 作業用データ置き場 (exe の隣)。%TEMP% は環境によって書き込みが拒否されることがあったので使わない。
pub fn data_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("yt-dlp.exe"));
    exe.parent().unwrap_or(Path::new(".")).join("vrc-video-proxy-data")
}

fn dir() -> PathBuf {
    data_dir().join("cache")
}

pub fn key(parts: &[&str]) -> String {
    let mut h = DefaultHasher::new();
    parts.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn age(p: &Path) -> Option<Duration> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    SystemTime::now().duration_since(m).ok()
}

/// ttl 以内のキャッシュがあれば返す。ローカル配信 URL はサーバが生きているかも確認する。
pub fn get(key: &str, ttl_secs: u64) -> Option<Vec<String>> {
    if ttl_secs == 0 {
        return None;
    }
    let p = dir().join(format!("{key}.txt"));
    if age(&p)? > Duration::from_secs(ttl_secs) {
        let _ = std::fs::remove_file(&p);
        return None;
    }
    let lines: Vec<String> = std::fs::read_to_string(&p).ok()?.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return None;
    }
    for l in &lines {
        if let Some(addr) = local_addr(l) {
            let ok = addr.parse().ok().and_then(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).ok()).is_some();
            if !ok {
                let _ = std::fs::remove_file(&p);
                return None;
            }
        }
    }
    Some(lines)
}

pub fn put(key: &str, ttl_secs: u64, lines: &[String]) {
    if ttl_secs == 0 {
        return;
    }
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    // ついでに期限切れを掃除
    if let Ok(rd) = std::fs::read_dir(&d) {
        for e in rd.flatten() {
            if age(&e.path()).is_some_and(|a| a > Duration::from_secs(ttl_secs.max(3600))) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::write(d.join(format!("{key}.txt")), lines.join("\n"));
}

/// "http://127.0.0.1:1234/stream" -> "127.0.0.1:1234"
fn local_addr(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("http://")?;
    let hostport = rest.split('/').next()?;
    (hostport.starts_with("127.") || hostport.starts_with("localhost")).then_some(hostport)
}
