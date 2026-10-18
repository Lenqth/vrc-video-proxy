//! ffmpeg の出力を HTTP で流すローカルサーバ。
//! yt-dlp.exe 本体とは別プロセス (デタッチ) で起動され、アイドルが続くと自動終了する。

use crate::buffered;
use crate::log::Logger;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SERVE_FLAG: &str = "--vrc-video-proxy-serve";
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Serialize, Deserialize, Clone)]
pub struct Job {
    pub host: String,
    pub port: u16,
    pub idle_timeout_secs: u64,
    pub ffmpeg: PathBuf,
    pub ffmpeg_args: Vec<String>,
    /// GPU エンコードに失敗したときに使う CPU 版の引数
    #[serde(default)]
    pub fallback_args: Option<Vec<String>>,
    pub working_dir: PathBuf,
    pub content_type: String,
    pub ffmpeg_log: PathBuf,
    /// Some の場合は ffmpeg を使わず、この URL をそのまま中継する
    #[serde(default)]
    pub relay: Option<Relay>,
    /// Some の場合は ffmpeg の出力をファイルに保存しながら Range 付きで配信する (シーク可)
    #[serde(default)]
    pub buffer: Option<Buffer>,
    /// Some の場合はこのローカルファイルを Range 対応で配信する (テスト動画)
    #[serde(default)]
    pub file: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Buffer {
    pub path: PathBuf,
    pub est_size: u64,
    /// 最初のリクエストで、取り終わるのをこの秒数まで待つ
    pub wait_secs: u64,
    /// 動画の長さ (秒)。断片化 mp4 の先頭に書き込む
    #[serde(default)]
    pub duration: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Relay {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub ipv4: bool,
    pub retries: u32,
}

fn make_agent(ipv4: bool) -> ureq::Agent {
    let b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .redirects(5);
    if ipv4 {
        b.resolver(|addr: &str| -> std::io::Result<Vec<SocketAddr>> { Ok(addr.to_socket_addrs()?.filter(|a| a.is_ipv4()).collect()) })
            .build()
    } else {
        b.build()
    }
}

struct State {
    active: AtomicUsize,
    last: Mutex<Instant>,
    conn_seq: AtomicUsize,
}

impl State {
    fn touch(&self) {
        *self.last.lock().unwrap() = Instant::now();
    }
}

struct ActiveGuard(Arc<State>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

pub fn run(job_path: &Path, logger: Logger) -> i32 {
    let job: Job = match std::fs::read_to_string(job_path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(j) => j,
        Err(e) => {
            logger.log(format!("failed to read job {}: {e}", job_path.display()));
            return 1;
        }
    };
    let _ = std::fs::remove_file(job_path);

    let listener = match TcpListener::bind((job.host.as_str(), job.port)) {
        Ok(l) => l,
        Err(e) => {
            logger.log(format!("bind {}:{} failed: {e}", job.host, job.port));
            return 1;
        }
    };
    logger.log(format!("serving on http://{}:{}/ (idle timeout {}s)", job.host, job.port, job.idle_timeout_secs));

    let state = Arc::new(State { active: AtomicUsize::new(0), last: Mutex::new(Instant::now()), conn_seq: AtomicUsize::new(0) });

    // 取りながら流すモードなら、接続を待たずにダウンロードを始める
    let buf_state = match &job.buffer {
        Some(b) => match buffered::start(&job, b.path.clone(), b.est_size, b.wait_secs, b.duration, &logger) {
            Ok(s) => Some(s),
            Err(e) => {
                logger.log(format!("failed to start buffering: {e}"));
                return 1;
            }
        },
        None => None,
    };

    {
        let state = state.clone();
        let logger = logger.clone();
        let idle = Duration::from_secs(job.idle_timeout_secs);
        let buf_path = job.buffer.as_ref().map(|b| b.path.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if state.active.load(Ordering::SeqCst) == 0 && state.last.lock().unwrap().elapsed() > idle {
                logger.log("idle timeout, exiting");
                if let Some(p) = &buf_path {
                    let _ = std::fs::remove_file(p);
                    let _ = std::fs::remove_file(p.with_extension("final.mp4"));
                }
                std::process::exit(0);
            }
        });
    }

    let agent = make_agent(job.relay.as_ref().is_some_and(|r| r.ipv4));
    let job = Arc::new(job);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        state.active.fetch_add(1, Ordering::SeqCst);
        state.touch();
        let guard = ActiveGuard(state.clone());
        let id = state.conn_seq.fetch_add(1, Ordering::SeqCst);
        let job = job.clone();
        let logger = logger.clone();
        let agent = agent.clone();
        let buf_state = buf_state.clone();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Err(e) = handle(stream, &job, &agent, buf_state.as_deref(), &logger, id) {
                logger.log(format!("conn#{id} error: {e}"));
            }
        });
    }
    0
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = match stream.read(&mut tmp) {
            Ok(0) => return Ok(None),
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 16 * 1024 {
            return Ok(None);
        }
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn handle(mut stream: TcpStream, job: &Job, agent: &ureq::Agent, buf: Option<&buffered::BufState>, logger: &Logger, id: usize) -> std::io::Result<()> {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let Some(req) = read_request(&mut stream)? else {
        // 起動確認の接続など
        return Ok(());
    };
    let mut lines = req.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let interesting: Vec<&str> = lines
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            l.starts_with("range:") || l.starts_with("user-agent:")
        })
        .collect();
    logger.log(format!("conn#{id} {peer} \"{request_line}\" {}", interesting.join(" | ")));

    let method = request_line.split_whitespace().next().unwrap_or("");
    let range = interesting
        .iter()
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
        .map(|l| l[6..].trim().to_string());
    if let Some(relay) = &job.relay {
        return handle_relay(stream, method, range.as_deref(), relay, agent, logger, id);
    }
    if let Some(st) = buf {
        return buffered::handle(stream, method, range.as_deref(), st, &job.content_type, logger, id);
    }
    if let Some(p) = &job.file {
        return buffered::serve_file(stream, method, range.as_deref(), p, &job.content_type, logger, id);
    }
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nCache-Control: no-cache\r\nAccept-Ranges: none\r\nConnection: close\r\n\r\n",
        job.content_type
    );
    match method {
        "HEAD" => {
            stream.write_all(header.as_bytes())?;
            return Ok(());
        }
        "GET" => {}
        _ => {
            stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        }
    }

    let spawn = |args: &[String]| -> std::io::Result<std::process::Child> {
        let stderr = std::fs::OpenOptions::new().create(true).append(true).open(&job.ffmpeg_log)?;
        {
            let mut f = &stderr;
            let _ = writeln!(f, "===== {} conn#{id} start =====", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
        }
        Command::new(&job.ffmpeg)
            .args(args)
            .current_dir(&job.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    };

    let mut child = spawn(&job.ffmpeg_args)?;
    let mut out = child.stdout.take().unwrap();
    let started = Instant::now();

    let mut buf = vec![0u8; 256 * 1024];
    let mut sent: u64 = 0;
    let mut first = out.read(&mut buf).unwrap_or(0);
    // GPU エンコードが何も出さずに失敗したら CPU でやり直す
    if first == 0 {
        if let Some(fb) = &job.fallback_args {
            let _ = child.kill();
            let _ = child.wait();
            logger.log(format!("conn#{id} ffmpeg produced no output, retrying with CPU encoder"));
            child = spawn(fb)?;
            out = child.stdout.take().unwrap();
            first = out.read(&mut buf).unwrap_or(0);
        }
    }
    let result = if first == 0 {
        let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        "ffmpeg produced no output (see ffmpeg log)".to_string()
    } else {
        let r = (|| -> std::io::Result<()> {
            stream.write_all(header.as_bytes())?;
            stream.write_all(&buf[..first])?;
            sent += first as u64;
            loop {
                let n = out.read(&mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                stream.write_all(&buf[..n])?;
                sent += n as u64;
            }
        })();
        match r {
            Ok(()) => "ffmpeg finished".to_string(),
            Err(e) => format!("client closed ({e})"),
        }
    };

    let _ = child.kill();
    let status = child.wait().ok();
    logger.log(format!(
        "conn#{id} done: {result}, sent {:.1} MiB in {:.1}s, ffmpeg exit {:?}",
        sent as f64 / 1048576.0,
        started.elapsed().as_secs_f64(),
        status.and_then(|s| s.code())
    ));
    Ok(())
}

const RELAY_HEADERS: &[&str] = &["content-type", "content-length", "content-range", "accept-ranges", "last-modified", "etag"];

fn handle_relay(
    mut stream: TcpStream,
    method: &str,
    range: Option<&str>,
    relay: &Relay,
    agent: &ureq::Agent,
    logger: &Logger,
    id: usize,
) -> std::io::Result<()> {
    let method = if method == "HEAD" { "HEAD" } else { "GET" };
    let started = Instant::now();
    let mut attempt = 0;
    let resp = loop {
        attempt += 1;
        let mut req = agent.request(method, &relay.url);
        for (k, v) in &relay.headers {
            req = req.set(k, v);
        }
        if let Some(r) = range {
            req = req.set("Range", r);
        }
        let retryable = match req.call() {
            Ok(resp) => break Ok(resp),
            Err(ureq::Error::Status(code, _)) if code == 403 || code >= 500 => format!("upstream {code}"),
            Err(ureq::Error::Status(code, _)) => break Err(code),
            Err(e) => format!("transport: {e}"),
        };
        if attempt > relay.retries {
            logger.log(format!("conn#{id} give up after {attempt} attempts: {retryable}"));
            break Err(502);
        }
        logger.log(format!("conn#{id} attempt {attempt} failed ({retryable}), retrying"));
        std::thread::sleep(Duration::from_millis(300 * attempt as u64));
    };

    let resp = match resp {
        Ok(r) => r,
        Err(code) => {
            logger.log(format!("conn#{id} -> {code}"));
            stream.write_all(format!("HTTP/1.1 {code} Upstream Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())?;
            return Ok(());
        }
    };

    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status(), resp.status_text());
    for h in RELAY_HEADERS {
        if let Some(v) = resp.header(h) {
            head.push_str(&format!("{h}: {v}\r\n"));
        }
    }
    head.push_str("Connection: close\r\n\r\n");
    let summary = format!(
        "conn#{id} upstream {} (attempt {attempt}) len={} range={}",
        resp.status(),
        resp.header("content-length").unwrap_or("-"),
        resp.header("content-range").unwrap_or("-")
    );
    logger.log(&summary);
    stream.write_all(head.as_bytes())?;
    if method == "HEAD" {
        return Ok(());
    }

    let mut reader = resp.into_reader();
    let result = std::io::copy(&mut reader, &mut stream);
    logger.log(format!(
        "conn#{id} done: {}, {:.1}s",
        match &result {
            Ok(n) => format!("sent {:.1} MiB", *n as f64 / 1048576.0),
            Err(e) => format!("stopped ({e})"),
        },
        started.elapsed().as_secs_f64()
    ));
    Ok(())
}
