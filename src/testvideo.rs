//! テスト用動画の生成 (設定 test_videos = true のときだけ有効)。
//!
//! 次のどちらかの URL で、ffmpeg で生成した指定コーデックの小さな動画を返す。
//! - テスト用ホスト:       https://vrc-video-proxy.test/?vcodec=hevc&acodec=opus
//!                          https://vrc-video-proxy.test/hevc/opus   (パス形式)
//! - 任意の URL + 目印:     https://www.youtube.com/watch?vrc_video_proxy_test&vcodec=vp9
//!   (VRChat は信頼済みドメイン以外を弾くことがあるので、YouTube の URL に目印を付ける形も使える)
//!
//! パラメータ: vcodec, acodec, dur (秒), res (WxH), fps, container (mp4/webm/mkv), raw (1 で変換しない)

use crate::log::Logger;
use crate::server::CREATE_NO_WINDOW;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

pub const MARKER: &str = "vrc_video_proxy_test";

#[derive(Debug, Clone)]
pub struct Spec {
    pub vcodec: String,
    pub acodec: String,
    pub dur: f64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub container: String,
    /// true なら許容コーデック判定・変換をせずにそのまま返す
    pub raw: bool,
}

/// 生成結果 (main 側で yt-dlp の情報と同じように扱う)
pub struct Generated {
    pub path: PathBuf,
    pub title: String,
    pub vcodec: String,
    pub acodec: String,
    pub duration: f64,
    pub size: u64,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// URL がテスト動画の指定なら Spec を返す
pub fn parse(url: &str, test_host: &str) -> Option<Spec> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let (authority, path_query) = match rest.find('/') {
        Some(p) => (&rest[..p], &rest[p..]),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, q.split('#').next().unwrap_or("")),
        None => (path_query.split('#').next().unwrap_or(""), ""),
    };
    let params: Vec<(String, String)> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (percent_decode(k).to_ascii_lowercase(), percent_decode(v)),
            None => (percent_decode(kv).to_ascii_lowercase(), String::new()),
        })
        .collect();
    let by_host = !test_host.is_empty() && host == test_host.to_ascii_lowercase();
    let by_marker = params.iter().any(|(k, _)| k == MARKER);
    if !by_host && !by_marker {
        return None;
    }
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.trim().to_ascii_lowercase());

    // パス形式 (/hevc/opus) はテスト用ホストのときだけ
    let segs: Vec<&str> = if by_host { path.split('/').filter(|s| !s.is_empty()).collect() } else { vec![] };
    let vcodec = get("vcodec").or_else(|| segs.first().map(|s| s.to_ascii_lowercase())).unwrap_or_else(|| "h264".into());
    let acodec = get("acodec").or_else(|| segs.get(1).map(|s| s.to_ascii_lowercase())).unwrap_or_else(|| "aac".into());
    let dur = get("dur").and_then(|v| v.parse::<f64>().ok()).unwrap_or(10.0).clamp(1.0, 600.0);
    let (width, height) = get("res")
        .and_then(|r| {
            let (w, h) = r.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1280u32, 720u32));
    let (width, height) = (width.clamp(16, 3840) & !1, height.clamp(16, 2160) & !1);
    let fps = get("fps").and_then(|v| v.parse().ok()).unwrap_or(30u32).clamp(1, 120);
    // mp4 に入らない組み合わせは webm にする
    let default_container = if matches!(vcodec.as_str(), "vp8") || acodec == "vorbis" { "webm" } else { "mp4" };
    let container = get("container").unwrap_or_else(|| default_container.into());
    let raw = get("raw").is_some_and(|v| v.is_empty() || v == "1" || v == "true");
    Some(Spec { vcodec, acodec, dur, width, height, fps, container, raw })
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// (エンコーダ引数, yt-dlp 風のコーデック名)。一覧に無い名前は ffmpeg のエンコーダ名として使う。
fn video_encoder(v: &str, mp4: bool) -> (Vec<String>, String) {
    match v {
        "h264" | "avc" | "avc1" => (strs(&["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"]), "avc1.640028".into()),
        "hevc" | "h265" | "hvc1" | "hev1" => {
            let mut a = strs(&["-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-x265-params", "log-level=error"]);
            if mp4 {
                a.extend(strs(&["-tag:v", "hvc1"]));
            }
            (a, "hvc1".into())
        }
        "av1" | "av01" => (strs(&["-c:v", "libsvtav1", "-preset", "12", "-pix_fmt", "yuv420p"]), "av01".into()),
        "vp9" | "vp09" => (strs(&["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-row-mt", "1", "-b:v", "2M", "-pix_fmt", "yuv420p"]), "vp09".into()),
        "vp8" => (strs(&["-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "2M"]), "vp8".into()),
        "none" => (strs(&["-vn"]), "none".into()),
        other => (vec!["-c:v".into(), other.into()], other.into()),
    }
}

fn audio_encoder(a: &str) -> (Vec<String>, String) {
    match a {
        "aac" | "mp4a" => (strs(&["-c:a", "aac", "-b:a", "128k"]), "mp4a.40.2".into()),
        "opus" => (strs(&["-c:a", "libopus", "-b:a", "128k"]), "opus".into()),
        "mp3" => (strs(&["-c:a", "libmp3lame", "-b:a", "128k"]), "mp3".into()),
        "vorbis" => (strs(&["-c:a", "libvorbis", "-q:a", "4"]), "vorbis".into()),
        "flac" => (strs(&["-c:a", "flac"]), "flac".into()),
        "none" => (strs(&["-an"]), "none".into()),
        other => (vec!["-c:a".into(), other.into()], other.into()),
    }
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        _ => "video/mp4",
    }
}

/// テスト動画を生成する (同じ指定なら前回のファイルを使い回す)
pub fn generate(spec: &Spec, ffmpeg: &Path, dir: &Path, logger: &Logger) -> Result<Generated, String> {
    let (ext, fmt) = match spec.container.as_str() {
        "webm" => ("webm", "webm"),
        "mkv" | "matroska" => ("mkv", "matroska"),
        _ => ("mp4", "mp4"),
    };
    let mp4 = ext == "mp4";
    let (vargs, vname) = video_encoder(&spec.vcodec, mp4);
    let (aargs, aname) = audio_encoder(&spec.acodec);
    if vname == "none" && aname == "none" {
        return Err("vcodec and acodec are both none".into());
    }
    let title = format!("test {}/{} {}x{} {}fps {}s .{ext}", spec.vcodec, spec.acodec, spec.width, spec.height, spec.fps, spec.dur);
    let key = crate::cache::key(&[&title]);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("test_{key}.{ext}"));

    if !path.exists() {
        let tmp = dir.join(format!("test_{key}.tmp.{ext}"));
        let dur = format!("{}", spec.dur);
        let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-y"].iter().map(|s| s.to_string()).collect();
        let mut idx = 0;
        let mut maps = Vec::new();
        if vname != "none" {
            args.extend(["-f".into(), "lavfi".into(), "-i".into(), format!("testsrc2=size={}x{}:rate={}", spec.width, spec.height, spec.fps)]);
            maps.extend(["-map".to_string(), format!("{idx}:v")]);
            idx += 1;
        }
        if aname != "none" {
            args.extend(["-f".into(), "lavfi".into(), "-i".into(), "sine=frequency=1000:beep_factor=4:sample_rate=48000".into()]);
            maps.extend(["-map".to_string(), format!("{idx}:a")]);
        }
        args.extend(maps);
        args.extend(["-t".into(), dur]);
        args.extend(vargs);
        args.extend(aargs);
        if aname != "none" {
            args.extend(["-ac".into(), "2".into()]);
        }
        if mp4 {
            args.extend(["-movflags".into(), "+faststart".into()]);
        }
        args.extend(["-f".into(), fmt.into()]);
        args.push(tmp.to_string_lossy().into_owned());

        logger.log(format!("test video: generating {title}"));
        let started = Instant::now();
        let out = Command::new(ffmpeg)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("failed to start ffmpeg: {e}"))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        logger.log(format!("test video: generated in {:.1}s", started.elapsed().as_secs_f64()));
    } else {
        logger.log(format!("test video: reuse {}", path.display()));
    }
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    Ok(Generated { path, title, vcodec: vname, acodec: aname, duration: spec.dur, size })
}
