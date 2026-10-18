mod buffered;
mod cache;
mod config;
mod gpu;
mod log;
mod server;
mod testvideo;

use config::{ArgsMode, Config, ConvertMode};
use log::Logger;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

#[derive(Deserialize, Debug, Default)]
struct Format {
    url: Option<String>,
    format_id: Option<String>,
    acodec: Option<String>,
    vcodec: Option<String>,
    protocol: Option<String>,
    #[serde(default)]
    http_headers: BTreeMap<String, String>,
    /// yt-dlp が付ける Set-Cookie 風の文字列 ("a=1; Domain=..; Path=/; b=2; ...")
    cookies: Option<String>,
    tbr: Option<f64>,
    filesize: Option<f64>,
    filesize_approx: Option<f64>,
    height: Option<f64>,
}

impl Format {
    /// cookies を Cookie ヘッダとして http_headers に入れる (ffmpeg / 中継でも認証が通るように)
    fn apply_cookies(&mut self) {
        let Some(c) = &self.cookies else { return };
        const ATTRS: &[&str] = &["domain", "path", "expires", "max-age", "secure", "httponly", "samesite", "version", "comment"];
        let pairs: Vec<&str> = c
            .split(';')
            .map(str::trim)
            .filter(|kv| kv.split_once('=').is_some_and(|(k, _)| !ATTRS.contains(&k.trim().to_ascii_lowercase().as_str())))
            .collect();
        if !pairs.is_empty() {
            self.http_headers.insert("Cookie".into(), pairs.join("; "));
        }
    }
}

#[derive(Deserialize, Debug, Default)]
struct Info {
    title: Option<String>,
    duration: Option<f64>,
    is_live: Option<bool>,
    #[serde(flatten)]
    top: Format,
    requested_formats: Option<Vec<Format>>,
}

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("yt-dlp.exe"));
    let base = exe.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let (cfg, warn) = Config::load(&base);

    let log_dir = if cfg.log_dir.is_empty() { base.join("vrc-video-proxy-logs") } else { Config::resolve(&base, &cfg.log_dir) };
    let pid = std::process::id();

    // インストールスクリプト等が「このプロキシか」を確認するため
    if args.first().map(String::as_str) == Some("--vrc-video-proxy-version") {
        println!("vrc-video-proxy {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    if args.first().map(String::as_str) == Some(server::SERVE_FLAG) {
        let logger = Logger::new(&log_dir, format!("serve {pid}"));
        let code = match args.get(1) {
            Some(p) => server::run(Path::new(p), logger),
            None => 1,
        };
        std::process::exit(code);
    }

    log::cleanup(&log_dir, cfg.log_keep_days);
    let logger = Logger::new(&log_dir, format!("{pid}"));
    if let Some(w) = warn {
        logger.log(w);
    }
    let code = run(&args, &cfg, &base, &log_dir, &logger);
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

struct Ctx<'a> {
    cfg: &'a Config,
    base: &'a Path,
    log_dir: &'a Path,
    logger: &'a Logger,
    workdir: PathBuf,
    ytdlp: PathBuf,
}

fn run(args: &[String], cfg: &Config, base: &Path, log_dir: &Path, logger: &Logger) -> i32 {
    let workdir = if cfg.working_dir.is_empty() { base.to_path_buf() } else { Config::resolve(base, &cfg.working_dir) };
    let ytdlp = Config::resolve_exe(base, &cfg.ytdlp_path);
    let ctx = Ctx { cfg, base, log_dir, logger, workdir, ytdlp };

    logger.log("==================================================");
    logger.log(format!("invoked: {}", quote_args(args)));
    logger.log(format!("cwd: {}", std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()));

    let is_resolve = args.iter().any(|a| a == "--get-url" || a == "-g");
    if !is_resolve {
        logger.log("not a --get-url request, passing through");
        return passthrough(&ctx, args.to_vec());
    }

    let url = args.iter().rev().find(|a| !a.starts_with('-') && a.contains("://")).cloned();
    logger.log(format!("target url: {}", url.as_deref().unwrap_or("(not found)")));

    let mut base_args: Vec<String> = match cfg.args_mode {
        ArgsMode::Append => args.iter().filter(|a| *a != "--get-url" && *a != "-g").cloned().chain(cfg.extra_args.iter().cloned()).collect(),
        ArgsMode::Replace => {
            let mut v = cfg.extra_args.clone();
            if let Some(u) = &url {
                v.push(u.clone());
            }
            v
        }
    };

    rewrite_format(&mut base_args, cfg, logger);

    let mode = format!("{:?}", cfg.convert);
    let ytdlp_str = ctx.ytdlp.to_string_lossy().into_owned();
    let mut key_parts: Vec<&str> = vec![&ytdlp_str, &mode];
    key_parts.extend(base_args.iter().map(String::as_str));
    let key = cache::key(&key_parts);
    if let Some(lines) = cache::get(&key, cfg.cache_ttl_secs) {
        logger.log(format!("cache hit ({key})"));
        for l in &lines {
            logger.log(format!("output: {l}"));
            println!("{l}");
        }
        return 0;
    }
    let emit = |lines: Vec<String>| -> i32 {
        for l in &lines {
            logger.log(format!("output: {l}"));
            println!("{l}");
        }
        cache::put(&key, cfg.cache_ttl_secs, &lines);
        0
    };

    let test_spec = if cfg.test_videos { url.as_deref().and_then(|u| testvideo::parse(u, &cfg.test_host)) } else { None };
    let raw = test_spec.as_ref().is_some_and(|s| s.raw);

    let info: Info = if let Some(spec) = &test_spec {
        // テスト動画: yt-dlp は使わず、ローカルで生成したファイルを yt-dlp の結果と同じように扱う
        logger.log(format!("test video request: {spec:?}"));
        let ffmpeg = Config::resolve_exe(base, &cfg.ffmpeg_path);
        match testvideo::generate(spec, &ffmpeg, &cache::data_dir().join("test"), logger) {
            Ok(g) => Info {
                title: Some(g.title),
                duration: Some(g.duration),
                is_live: Some(false),
                top: Format {
                    url: Some(g.path.to_string_lossy().into_owned()),
                    format_id: Some("test".into()),
                    vcodec: Some(g.vcodec),
                    acodec: Some(g.acodec),
                    protocol: Some("file".into()),
                    filesize: Some(g.size as f64),
                    height: Some(spec.height as f64),
                    ..Default::default()
                },
                requested_formats: None,
            },
            Err(e) => {
                logger.log(format!("test video failed: {e}"));
                eprintln!("ERROR: vrc-video-proxy: test video failed: {e}");
                return 1;
            }
        }
    } else {
        if cfg.convert == ConvertMode::Never {
            base_args.push("--get-url".into());
            let Some((stdout, code)) = run_ytdlp(&ctx, &base_args) else { return 1 };
            if code != 0 {
                return code;
            }
            return emit(stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect());
        }

        base_args.push("-j".into());
        let Some((stdout, code)) = run_ytdlp(&ctx, &base_args) else { return 1 };
        if code != 0 {
            return code;
        }

        let Some(line) = stdout.lines().find(|l| l.trim_start().starts_with('{')) else {
            logger.log("no JSON in yt-dlp output");
            eprintln!("ERROR: vrc-video-proxy: no JSON in yt-dlp output");
            return 1;
        };
        match serde_json::from_str(line) {
            Ok(i) => i,
            Err(e) => {
                logger.log(format!("JSON parse error: {e}"));
                eprintln!("ERROR: vrc-video-proxy: JSON parse error: {e}");
                return 1;
            }
        }
    };

    let mut formats: Vec<Format> = match info.requested_formats {
        Some(v) if !v.is_empty() => v,
        _ => vec![info.top],
    };
    formats.iter_mut().for_each(Format::apply_cookies);
    logger.log(format!("title: {}", info.title.as_deref().unwrap_or("?")));
    for f in &formats {
        logger.log(format!(
            "format {}: vcodec={} acodec={} protocol={}",
            f.format_id.as_deref().unwrap_or("?"),
            f.vcodec.as_deref().unwrap_or("?"),
            f.acodec.as_deref().unwrap_or("?"),
            f.protocol.as_deref().unwrap_or("?")
        ));
    }
    let urls: Vec<String> = formats.iter().filter_map(|f| f.url.clone()).collect();
    if urls.is_empty() {
        logger.log("no url in formats");
        eprintln!("ERROR: vrc-video-proxy: no url in yt-dlp output");
        return 1;
    }

    let (need, reason) = if raw { (false, "raw test video".into()) } else { need_convert(cfg, &formats) };
    logger.log(format!("convert: {need} ({reason})"));

    if need {
        // 映像を変換するときだけエンコーダを選ぶ (GPU 検出は初回のみ数百 ms)
        let (vtrans, vfallback, maxrate) = if formats.iter().all(|f| video_allowed(cfg, f)) {
            (vec![], None, None)
        } else {
            let height = formats.iter().filter_map(|f| f.height).fold(None, |m: Option<f64>, h| Some(m.map_or(h, |m| m.max(h))));
            let sel = gpu::select(cfg, &Config::resolve_exe(base, &cfg.ffmpeg_path), &cache::data_dir(), height, logger);
            logger.log(format!("video encoder: {} (maxrate {:?} kbps)", sel.name, sel.maxrate));
            (sel.args, sel.fallback, sel.maxrate)
        };
        let ffmpeg_args = build_ffmpeg_args(cfg, &formats, &vtrans);
        let fallback_args = vfallback.map(|fb| build_ffmpeg_args(cfg, &formats, &fb));
        // 長さが分かる (ライブでない) なら、取りながら流してシークできるようにする
        let est = if cfg.seekable_convert && !info.is_live.unwrap_or(false) { estimate_size(cfg, info.duration, &formats, maxrate) } else { None };
        let (buffer, idle, path) = match est {
            Some(est_size) => {
                let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let p = cache::data_dir().join("media").join(format!("{stamp}_{}.mp4", std::process::id()));
                (Some(server::Buffer { path: p, est_size, wait_secs: cfg.buffer_wait_secs, duration: info.duration }), cfg.relay_idle_timeout_secs, "video.mp4")
            }
            None => (None, cfg.idle_timeout_secs, "stream"),
        };
        logger.log(format!("seekable: {}", buffer.is_some()));
        match start_server(&ctx, Serve::Ffmpeg { args: ffmpeg_args, fallback: fallback_args, buffer }, idle, path) {
            Ok(local) => return emit(vec![local]),
            Err(e) => {
                logger.log(format!("failed to start server: {e}, falling back to raw url"));
            }
        }
    } else if formats.len() == 1 && !urls[0].contains("://") {
        // ローカルファイル (テスト動画) はそのまま配信
        let p = PathBuf::from(&urls[0]);
        let name = format!("video.{}", p.extension().and_then(|e| e.to_str()).unwrap_or("mp4"));
        match start_server(&ctx, Serve::File(p), cfg.relay_idle_timeout_secs, &name) {
            Ok(local) => return emit(vec![local]),
            Err(e) => {
                logger.log(format!("failed to start file server: {e}"));
                eprintln!("ERROR: vrc-video-proxy: failed to start file server: {e}");
                return 1;
            }
        }
    } else if cfg.relay && formats.len() == 1 && urls[0].starts_with("https://") {
        let relay = server::Relay {
            url: urls[0].clone(),
            headers: formats[0].http_headers.clone(),
            ipv4: cfg.relay_ipv4,
            retries: cfg.relay_retries,
        };
        logger.log(format!("relay: ipv4={} retries={}", relay.ipv4, relay.retries));
        // HLS (ライブ) はプレイヤーが拡張子で判別するので .m3u8 にする
        let is_hls = formats[0].protocol.as_deref().is_some_and(|p| p.starts_with("m3u8"));
        let path = if is_hls { "video.m3u8" } else { "video.mp4" };
        match start_server(&ctx, Serve::Relay(relay), cfg.relay_idle_timeout_secs, path) {
            Ok(local) => return emit(vec![local]),
            Err(e) => {
                logger.log(format!("failed to start relay: {e}, falling back to raw url"));
            }
        }
    }
    emit(urls)
}

/// 許容コーデック一覧から yt-dlp のフィルタ用正規表現を作る ("^(avc1|h264)")
fn codec_regex(list: &[String]) -> Option<String> {
    let items: Vec<String> = list
        .iter()
        .map(|c| c.trim().chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '-' || *ch == '_').collect::<String>())
        .filter(|c| !c.is_empty())
        .map(|c| c.replace('.', "\\."))
        .collect();
    (!items.is_empty()).then(|| format!("^({})", items.join("|")))
}

/// VRChat の -f を書き換える。
/// - live: VRChat は最初 `[protocol^=http]` 付きで呼び、ライブ (HLS のみ) だと失敗してから
///   フィルタ無しで呼び直してくる。1 回で済むようにフィルタ無しを候補に足す。
/// - prefer_allowed_codecs: 許容コーデックの形式を優先する (変換が要らなくなる)。
/// - prefer_audio: 音声付きの形式を優先し、無ければ映像+音声を別々に取る (プロキシ側で ffmpeg 結合)。
///   ニコニコのように映像と音声が完全に分かれているサイトで音が出なくなるのを防ぐ。
fn rewrite_format(args: &mut [String], cfg: &Config, logger: &Logger) {
    const FILTER: &str = "[protocol^=http]";
    let vre = if cfg.prefer_allowed_codecs { codec_regex(&cfg.allowed_video_codecs) } else { None };
    let are = if cfg.prefer_allowed_codecs { codec_regex(&cfg.allowed_audio_codecs) } else { None };
    let vf = vre.map(|r| format!("[vcodec~='{r}']")).unwrap_or_default();
    let af = are.map(|r| format!("[acodec~='{r}']")).unwrap_or_default();
    for i in 0..args.len().saturating_sub(1) {
        if args[i] != "-f" && args[i] != "--format" {
            continue;
        }
        let orig = args[i + 1].clone();
        let noproto = orig.replace(FILTER, "");
        let mut bases = vec![orig.clone()];
        if cfg.live_fallback && noproto != orig {
            bases.push(noproto.clone());
        }
        // "(mp4/best)[height<=?1080]..." の後ろのフィルタ部分を結合形式にも付ける
        let filters = noproto.strip_prefix('(').and_then(|s| s.find(')').map(|p| s[p + 1..].to_string())).unwrap_or_default();

        let mut alts = Vec::new();
        // 1. 映像も音声も許容コーデックの、1 本になっている形式 (変換不要)
        if !vf.is_empty() || !af.is_empty() {
            let need_audio = if af.is_empty() { "[acodec!=none]".to_string() } else { af.clone() };
            for b in &bases {
                alts.push(format!("({b}){vf}{need_audio}"));
            }
        }
        if cfg.prefer_audio {
            // 2. 音声付きの形式 (コーデック問わず)
            for b in &bases {
                alts.push(format!("({b})[acodec!=none]"));
            }
            // 3. 映像+音声を別々に (許容コーデック優先)
            let merged = if vf.is_empty() && af.is_empty() { "bv*+ba".to_string() } else { format!("bv*{vf}+ba{af}/bv*{vf}+ba/bv*+ba") };
            alts.push(format!("({merged}){filters}"));
        }
        // 4. 元の指定
        alts.extend(bases);
        args[i + 1] = alts.join("/");
        if args[i + 1] != orig {
            logger.log(format!("format rewritten: {}", args[i + 1]));
        }
    }
}

fn has_audio(f: &Format) -> bool {
    !matches!(f.acodec.as_deref(), Some("none"))
}

fn has_video(f: &Format) -> bool {
    !matches!(f.vcodec.as_deref(), Some("none"))
}

/// コーデックが許容一覧に入っているか (前方一致、大文字小文字無視)。
/// 一覧が空、またはコーデック不明なら許容扱い。
fn codec_allowed(codec: Option<&str>, list: &[String]) -> bool {
    let Some(c) = codec else { return true };
    if c == "none" || list.is_empty() {
        return true;
    }
    let c = c.to_ascii_lowercase();
    list.iter().any(|a| c.starts_with(&a.trim().to_ascii_lowercase()))
}

fn video_allowed(cfg: &Config, f: &Format) -> bool {
    !has_video(f) || codec_allowed(f.vcodec.as_deref(), &cfg.allowed_video_codecs)
}

fn audio_allowed(cfg: &Config, f: &Format) -> bool {
    !has_audio(f) || codec_allowed(f.acodec.as_deref(), &cfg.allowed_audio_codecs)
}

fn need_convert(cfg: &Config, formats: &[Format]) -> (bool, String) {
    match cfg.convert {
        ConvertMode::Always => return (true, "convert=always".into()),
        ConvertMode::Never => return (false, "convert=never".into()),
        ConvertMode::Auto => {}
    }
    let mut reasons = Vec::new();
    if formats.len() > 1 {
        reasons.push(format!("{} separate streams, need merge", formats.len()));
    }
    for f in formats {
        if !video_allowed(cfg, f) {
            reasons.push(format!("video {} not allowed", f.vcodec.as_deref().unwrap_or("?")));
        }
        if !audio_allowed(cfg, f) {
            reasons.push(format!("audio {} not allowed", f.acodec.as_deref().unwrap_or("?")));
        }
    }
    if reasons.is_empty() {
        let v = formats[0].vcodec.as_deref().unwrap_or("?");
        let a = formats[0].acodec.as_deref().unwrap_or("?");
        (false, format!("codecs allowed (video {v}, audio {a})"))
    } else {
        (true, reasons.join(", "))
    }
}

/// video_transcode: 映像を変換する場合のエンコーダ引数 (許容コーデックならコピーするので使わない)
fn build_ffmpeg_args(cfg: &Config, formats: &[Format], video_transcode: &[String]) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "warning"].iter().map(|s| s.to_string()).collect();
    let inputs: Vec<&Format> = formats.iter().filter(|f| f.url.is_some()).collect();
    for f in &inputs {
        let is_http = f.url.as_deref().is_some_and(|u| u.starts_with("http"));
        if is_http {
            a.extend(cfg.ffmpeg_input_args.iter().cloned());
            if !f.http_headers.is_empty() {
                let h: String = f.http_headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                a.push("-headers".into());
                a.push(h);
            }
        }
        a.push("-i".into());
        a.push(f.url.clone().unwrap());
    }
    let mut video_mapped = false;
    let mut audio_mapped = false;
    for (i, f) in inputs.iter().enumerate() {
        if has_video(f) && !video_mapped {
            a.extend(["-map".into(), format!("{i}:v:0?")]);
            video_mapped = true;
        }
        if has_audio(f) && !audio_mapped {
            a.extend(["-map".into(), format!("{i}:a:0?")]);
            audio_mapped = true;
        }
    }
    // 許容コーデックはそのままコピー、それ以外だけ変換する
    if inputs.iter().all(|f| video_allowed(cfg, f)) {
        a.extend(cfg.ffmpeg_video_args.iter().cloned());
    } else {
        a.extend(video_transcode.iter().cloned());
    }
    if inputs.iter().all(|f| audio_allowed(cfg, f)) {
        a.extend(["-c:a".into(), "copy".into()]);
    } else {
        a.extend(cfg.ffmpeg_audio_args.iter().cloned());
    }
    a.extend(cfg.ffmpeg_output_args.iter().cloned());
    a.push("pipe:1".into());
    a
}

/// 出力サイズの推定 (多めに見積もる。足りない分はゼロ埋めになるが、超えると末尾が欠ける)
/// maxrate: 映像を変換する場合のビットレート上限 (kbps)。None で変換の場合は元サイズから大きめに見積もる
fn estimate_size(cfg: &Config, duration: Option<f64>, formats: &[Format], maxrate: Option<u32>) -> Option<u64> {
    let dur = duration.filter(|d| *d > 0.0)?;
    let mut bytes = 0.0;
    for f in formats {
        let src = match (f.filesize.or(f.filesize_approx), f.tbr) {
            (Some(s), _) => Some(s),
            (None, Some(tbr)) => Some(tbr * 1000.0 / 8.0 * dur),
            (None, None) => None,
        };
        bytes += if !video_allowed(cfg, f) {
            match maxrate {
                // 変換後の映像は上限ビットレートを超えない。同じ形式に音声があればその分も足す
                Some(kbps) => kbps as f64 * 1000.0 / 8.0 * dur * 1.1 + if has_audio(f) { 320_000.0 / 8.0 * dur } else { 0.0 },
                None => src? * 2.5,
            }
        } else {
            src? * 1.25
        };
    }
    // コンテナのオーバーヘッド + 音声を AAC 192k に変換した場合の増加分 + 余裕
    let audio = 192_000.0 / 8.0 * dur;
    Some((bytes + audio + 2.0 * 1048576.0) as u64)
}

/// ローカルサーバで何を配信するか
enum Serve {
    /// ffmpeg の出力 (buffer があれば保存しながら Range 対応で配信)。
    /// fallback は GPU エンコードに失敗したときに使う CPU 版の引数
    Ffmpeg { args: Vec<String>, fallback: Option<Vec<String>>, buffer: Option<server::Buffer> },
    /// 外部 URL をそのまま中継
    Relay(server::Relay),
    /// ローカルファイル (テスト動画など)
    File(PathBuf),
}

fn start_server(ctx: &Ctx, serve: Serve, idle_timeout_secs: u64, path: &str) -> Result<String, String> {
    let cfg = ctx.cfg;
    let port = TcpListener::bind((cfg.listen_host.as_str(), 0))
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("port probe: {e}"))?
        .port();
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let pid = std::process::id();
    let is_ffmpeg = matches!(serve, Serve::Ffmpeg { .. });
    let (ffmpeg_args, fallback_args, buffer, relay, file) = match serve {
        Serve::Ffmpeg { args, fallback, buffer } => (args, fallback, buffer, None, None),
        Serve::Relay(r) => (vec![], None, None, Some(r), None),
        Serve::File(p) => (vec![], None, None, None, Some(p)),
    };
    let content_type = match &file {
        Some(p) => testvideo::content_type(p).to_string(),
        None => cfg.content_type.clone(),
    };
    let job = server::Job {
        host: cfg.listen_host.clone(),
        port,
        idle_timeout_secs,
        ffmpeg: Config::resolve_exe(ctx.base, &cfg.ffmpeg_path),
        ffmpeg_args,
        fallback_args,
        working_dir: ctx.workdir.clone(),
        content_type,
        ffmpeg_log: ctx.log_dir.join(format!("ffmpeg_{stamp}_{pid}.log")),
        relay,
        buffer,
        file,
    };
    if is_ffmpeg {
        ctx.logger.log(format!("ffmpeg: {} {}", job.ffmpeg.display(), quote_args(&redact(&job.ffmpeg_args))));
        ctx.logger.log(format!("ffmpeg log: {}", job.ffmpeg_log.display()));
    }

    let job_dir = cache::data_dir().join("jobs");
    let _ = std::fs::create_dir_all(&job_dir);
    let job_path = job_dir.join(format!("job_{pid}_{port}.json"));
    std::fs::write(&job_path, serde_json::to_vec(&job).unwrap()).map_err(|e| format!("write job: {e}"))?;

    // VRChat から受け取った stdout/stderr のパイプをサーバに継承させない
    // (継承すると VRChat 側が EOF を受け取れず、サーバ終了まで待たされる)
    disable_std_handle_inherit();

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let spawn = |flags: u32| {
        Command::new(&exe)
            .arg(server::SERVE_FLAG)
            .arg(&job_path)
            .current_dir(&ctx.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    let base_flags = server::CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    // VRChat がジョブオブジェクトでプロセスツリーを kill しても生き残るように breakaway を試す
    let child = spawn(base_flags | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(base_flags)).map_err(|e| format!("spawn server: {e}"))?;
    ctx.logger.log(format!("server pid {}", child.id()));

    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = format!("{}:{}", cfg.listen_host, port);
    while Instant::now() < deadline {
        if TcpStream::connect(&addr).is_ok() {
            return Ok(format!("http://{addr}/{path}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("server did not start listening within 5s".into())
}

fn run_ytdlp(ctx: &Ctx, args: &[String]) -> Option<(String, i32)> {
    if ctx.cfg.ytdlp_path.trim().is_empty() {
        let msg = format!("ytdlp_path is not set in {}", ctx.base.join(config::CONFIG_NAME).display());
        ctx.logger.log(&msg);
        eprintln!("ERROR: vrc-video-proxy: {msg}");
        return None;
    }
    ctx.logger.log(format!("exec: {} {}", ctx.ytdlp.display(), quote_args(args)));
    ctx.logger.log(format!("workdir: {}", ctx.workdir.display()));
    let started = Instant::now();
    let out = Command::new(&ctx.ytdlp)
        .args(args)
        .current_dir(&ctx.workdir)
        .stdin(Stdio::null())
        .creation_flags(server::CREATE_NO_WINDOW)
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            ctx.logger.log(format!("failed to start yt-dlp: {e}"));
            eprintln!("ERROR: vrc-video-proxy: failed to start {}: {e}", ctx.ytdlp.display());
            return None;
        }
    };
    let code = out.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    ctx.logger.log(format!("yt-dlp exit {code} in {:.2}s", started.elapsed().as_secs_f64()));
    if !stderr.trim().is_empty() {
        ctx.logger.log(format!("--- yt-dlp stderr ---\n{}\n--- end stderr ---", stderr.trim_end()));
    }
    // VRChat 側のログにもエラーが出るように stderr は転送
    let _ = std::io::stderr().write_all(&out.stderr);
    Some((stdout, code))
}

fn passthrough(ctx: &Ctx, args: Vec<String>) -> i32 {
    let Some((stdout, code)) = run_ytdlp(ctx, &args) else { return 1 };
    let shown: String = stdout.chars().take(4000).collect();
    if !shown.trim().is_empty() {
        ctx.logger.log(format!("--- yt-dlp stdout ---\n{}\n--- end stdout ---", shown.trim_end()));
    }
    print!("{stdout}");
    code
}

fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| if a.is_empty() || a.contains([' ', '"', '&', '\t']) { format!("\"{}\"", a.replace('"', "\\\"")) } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// ログに Cookie ヘッダを残さない
fn redact(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| {
            if a.contains("\r\n") {
                a.split("\r\n")
                    .filter(|l| !l.is_empty())
                    .map(|l| if l.to_ascii_lowercase().starts_with("cookie:") { "Cookie: <redacted>".to_string() } else { l.to_string() })
                    .collect::<Vec<_>>()
                    .join("\\r\\n")
            } else {
                a.clone()
            }
        })
        .collect()
}

fn disable_std_handle_inherit() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: u32) -> isize;
        fn SetHandleInformation(h: isize, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    for n in [-10i32, -11, -12] {
        unsafe {
            let h = GetStdHandle(n as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
