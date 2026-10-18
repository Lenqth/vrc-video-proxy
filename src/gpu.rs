//! 映像変換に使うエンコーダの選択。
//!
//! video_encoder = "auto" なら、GPU エンコーダ (NVIDIA → AMD → Intel の順) で実際に小さな映像を
//! エンコードしてみて、成功したものを使う。どれも使えなければ CPU (libx264)。
//! 結果は data_dir に保存し、ffmpeg が変わるか一定期間経つまで再検出しない。

use crate::config::Config;
use crate::log::Logger;
use crate::server::CREATE_NO_WINDOW;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RECHECK: Duration = Duration::from_secs(7 * 24 * 3600);

/// 検出の優先順
const GPU_ORDER: &[&str] = &["nvenc", "amf", "qsv"];

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// 解像度ごとのビットレート上限 (kbps)。
/// 上限を付けないと GPU エンコーダは CPU よりかなり大きく出すことがあり、
/// 取りながら配信するときに名乗る推定サイズを超えて末尾が欠けてしまう。
pub fn maxrate_kbps(height: Option<f64>) -> u32 {
    match height.unwrap_or(1080.0) as u32 {
        0..=360 => 1500,
        361..=480 => 2500,
        481..=720 => 5000,
        721..=1080 => 8000,
        1081..=1440 => 12000,
        _ => 20000,
    }
}

/// エンコーダ名ごとの ffmpeg 引数 (品質は CPU の crf 23 前後に合わせ、maxrate で上限を付ける)
pub fn encoder_args(name: &str, maxrate: u32) -> Option<Vec<String>> {
    let target = format!("{}k", maxrate * 6 / 10);
    let max = format!("{maxrate}k");
    let buf = format!("{}k", maxrate * 2);
    let mut a = match name {
        "nvenc" => strs(&["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "23", "-b:v", &target]),
        "amf" => strs(&["-c:v", "h264_amf", "-quality", "balanced", "-rc", "vbr_peak", "-b:v", &target]),
        "qsv" => strs(&["-c:v", "h264_qsv", "-preset", "veryfast", "-b:v", &target]),
        "cpu" => strs(&["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"]),
        _ => return None,
    };
    a.extend(strs(&["-maxrate", &max, "-bufsize", &buf, "-pix_fmt", if name == "qsv" { "nv12" } else { "yuv420p" }]));
    Some(a)
}

/// 選ばれたエンコーダ
pub struct Selected {
    pub name: String,
    pub args: Vec<String>,
    /// GPU を使う場合、失敗したときに使う CPU の引数
    pub fallback: Option<Vec<String>>,
    /// 映像ビットレートの上限 (kbps)。引数を直接指定した場合は不明
    pub maxrate: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    ffmpeg: String,
    encoder: String,
    checked: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 実際に 0.2 秒の映像をエンコードしてみる
fn probe(ffmpeg: &Path, name: &str) -> bool {
    let Some(args) = encoder_args(name, 2000) else { return false };
    Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=256x144:r=30:d=0.2"])
        .args(&args)
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success())
}

fn detect(ffmpeg: &Path, data_dir: &Path, logger: &Logger) -> String {
    let cache_path = data_dir.join("encoder.json");
    let ffmpeg_str = ffmpeg.to_string_lossy().into_owned();
    if let Some(c) = std::fs::read_to_string(&cache_path).ok().and_then(|s| serde_json::from_str::<Cached>(&s).ok()) {
        if c.ffmpeg == ffmpeg_str && now().saturating_sub(c.checked) < RECHECK.as_secs() {
            return c.encoder;
        }
    }
    let mut found = "cpu".to_string();
    for name in GPU_ORDER {
        let ok = probe(ffmpeg, name);
        logger.log(format!("encoder probe: {name} -> {}", if ok { "ok" } else { "unavailable" }));
        if ok {
            found = name.to_string();
            break;
        }
    }
    logger.log(format!("encoder detected: {found}"));
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&cache_path, serde_json::to_vec(&Cached { ffmpeg: ffmpeg_str, encoder: found.clone(), checked: now() }).unwrap_or_default());
    found
}

/// 設定に従って映像変換のエンコーダを選ぶ (height は元動画の高さ。ビットレート上限に使う)
pub fn select(cfg: &Config, ffmpeg: &Path, data_dir: &Path, height: Option<f64>, logger: &Logger) -> Selected {
    // 引数を直接指定していればそれを使う
    if !cfg.video_transcode_args.is_empty() {
        return Selected { name: "custom".into(), args: cfg.video_transcode_args.clone(), fallback: None, maxrate: None };
    }
    let maxrate = maxrate_kbps(height);
    let mode = cfg.video_encoder.trim().to_ascii_lowercase();
    let name = if mode == "auto" { detect(ffmpeg, data_dir, logger) } else { mode };
    let cpu = encoder_args("cpu", maxrate).unwrap();
    match encoder_args(&name, maxrate) {
        Some(args) => {
            let fallback = (name != "cpu").then_some(cpu);
            Selected { name, args, fallback, maxrate: Some(maxrate) }
        }
        None => {
            logger.log(format!("unknown video_encoder '{name}', using cpu"));
            Selected { name: "cpu".into(), args: cpu, fallback: None, maxrate: Some(maxrate) }
        }
    }
}
