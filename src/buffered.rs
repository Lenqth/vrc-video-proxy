//! 「取りながら流す」モード。
//! ffmpeg の出力をファイルに保存し、Range 付きリクエストに答えてシークできるようにする。
//!
//! - 基本は断片化 mp4 を取りながら配信する。全体サイズは推定値を名乗り、足りない分の末尾はゼロで埋める。
//!   断片化 mp4 は先頭に長さが入らない (プレイヤーがライブ扱いする) ので、moov に長さを書き込む。
//! - wait_secs > 0 なら、最初のリクエストでその秒数まで取り終わるのを待ち、間に合えば
//!   普通の mp4 (faststart) に作り直して正確なサイズで配信する。

use crate::log::Logger;
use crate::server::{Job, CREATE_NO_WINDOW};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CHUNK: usize = 256 * 1024;
/// 末尾付近 (この範囲) へのリクエストは、ダウンロード中なら待たずにゼロを返す
const TAIL_PROBE: u64 = 4 * 1024 * 1024;
/// データ待ちでこの時間進展が無ければ諦める
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct BufState {
    pub path: PathBuf,
    pub final_path: PathBuf,
    pub est_size: u64,
    wait: Duration,
    started: Instant,
    written: AtomicU64,
    done: AtomicBool,
    /// 作り直した mp4 のサイズ (0 = まだ)
    final_size: AtomicU64,
    remux_failed: AtomicBool,
    /// 配信方式の決定: None = 未決定, Some(true) = 完成ファイル, Some(false) = 推定サイズ
    mode: Mutex<Option<bool>>,
}

impl BufState {
    fn written(&self) -> u64 {
        self.written.load(Ordering::SeqCst)
    }
    fn done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
    fn final_size(&self) -> u64 {
        self.final_size.load(Ordering::SeqCst)
    }

    /// 最初のリクエストで配信方式を決める (完成を最大 wait まで待つ)
    fn decide(&self, logger: &Logger) -> bool {
        loop {
            {
                let mut m = self.mode.lock().unwrap();
                if let Some(v) = *m {
                    return v;
                }
                if self.final_size() > 0 {
                    logger.log(format!("mode: complete file ({:.1} MiB)", self.final_size() as f64 / 1048576.0));
                    *m = Some(true);
                    return true;
                }
                // 時間切れ、または作り直しに失敗した
                if self.started.elapsed() > self.wait || self.remux_failed.load(Ordering::SeqCst) {
                    logger.log(format!("mode: growing file with estimated size (have {:.1} MiB)", self.written() as f64 / 1048576.0));
                    *m = Some(false);
                    return false;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// ffmpeg を起動してファイルへの書き出しを始める
pub fn start(job: &Job, path: PathBuf, est_size: u64, wait_secs: u64, duration: Option<f64>, logger: &Logger) -> std::io::Result<Arc<BufState>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = File::create(&path)?;
    let spawn = {
        let job = job.clone();
        move |args: &[String]| -> std::io::Result<Child> {
            let stderr = std::fs::OpenOptions::new().create(true).append(true).open(&job.ffmpeg_log)?;
            Command::new(&job.ffmpeg)
                .args(args)
                .current_dir(&job.working_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(stderr))
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
        }
    };
    let mut child = spawn(&job.ffmpeg_args)?;
    let fallback = job.fallback_args.clone();
    let final_path = path.with_extension("final.mp4");
    let st = Arc::new(BufState {
        path,
        final_path,
        est_size,
        wait: Duration::from_secs(wait_secs),
        started: Instant::now(),
        written: AtomicU64::new(0),
        done: AtomicBool::new(false),
        final_size: AtomicU64::new(0),
        remux_failed: AtomicBool::new(false),
        mode: Mutex::new(None),
    });
    logger.log(format!("buffering to {} (estimated {:.1} MiB, wait {wait_secs}s)", st.path.display(), est_size as f64 / 1048576.0));

    let st2 = st.clone();
    let logger = logger.clone();
    let ffmpeg = job.ffmpeg.clone();
    let ffmpeg_log = job.ffmpeg_log.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut status = pump(&mut child, &mut file, &st2, duration, &logger);
        // GPU エンコードが何も出さずに失敗したら CPU でやり直す
        if status != Some(0) && st2.written() == 0 {
            if let Some(fb) = &fallback {
                logger.log(format!("ffmpeg failed without output (exit {status:?}), retrying with CPU encoder"));
                match spawn(fb) {
                    Ok(mut c) => status = pump(&mut c, &mut file, &st2, duration, &logger),
                    Err(e) => logger.log(format!("failed to start fallback ffmpeg: {e}")),
                }
            }
        }
        let _ = file.flush();
        drop(file);
        st2.done.store(true, Ordering::SeqCst);
        let w = st2.written();
        logger.log(format!(
            "buffer done: {:.1} MiB in {:.1}s (estimated {:.1} MiB{}), ffmpeg exit {status:?}",
            w as f64 / 1048576.0,
            started.elapsed().as_secs_f64(),
            st2.est_size as f64 / 1048576.0,
            if w > st2.est_size { ", EXCEEDED estimate" } else { "" }
        ));

        // まだ配信方式が決まっていなければ、普通の mp4 に作り直す
        if status != Some(0) {
            st2.remux_failed.store(true, Ordering::SeqCst);
        } else if st2.mode.lock().unwrap().is_none() {
            let t = Instant::now();
            match remux(&ffmpeg, &st2.path, &st2.final_path, &ffmpeg_log) {
                Ok(size) => {
                    st2.final_size.store(size, Ordering::SeqCst);
                    logger.log(format!("remuxed to faststart mp4: {:.1} MiB in {:.1}s", size as f64 / 1048576.0, t.elapsed().as_secs_f64()));
                }
                Err(e) => {
                    st2.remux_failed.store(true, Ordering::SeqCst);
                    logger.log(format!("remux failed: {e}"));
                }
            }
        }
    });
    Ok(st)
}

/// ffmpeg の出力をファイルに書き出す。先頭の ftyp+moov が揃うまで溜めて、長さを書き込んでから出す。
/// 戻り値は ffmpeg の終了コード。
fn pump(child: &mut Child, file: &mut File, st: &BufState, duration: Option<f64>, logger: &Logger) -> Option<i32> {
    let Some(mut out) = child.stdout.take() else { return None };
    let mut buf = vec![0u8; CHUNK];
    let mut header: Option<Vec<u8>> = duration.map(|_| Vec::new());
    let mut write = |data: &[u8]| -> bool {
        if file.write_all(data).is_err() {
            return false;
        }
        st.written.fetch_add(data.len() as u64, Ordering::SeqCst);
        true
    };
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let Some(h) = header.as_mut() else {
            if !write(&buf[..n]) {
                break;
            }
            continue;
        };
        h.extend_from_slice(&buf[..n]);
        let data = match header_len(h) {
            Some(hl) => {
                let h = header.take().unwrap();
                let dur = duration.unwrap();
                match patch_duration(&h[..hl], dur) {
                    Some(mut p) => {
                        logger.log(format!("patched duration {dur:.1}s into moov"));
                        p.extend_from_slice(&h[hl..]);
                        p
                    }
                    None => {
                        logger.log("failed to patch duration");
                        h
                    }
                }
            }
            // moov が見つからない: 諦めてそのまま出す
            None if h.len() > 4 * 1024 * 1024 => {
                logger.log("moov not found in first 4 MiB, duration not patched");
                header.take().unwrap()
            }
            None => continue,
        };
        if !write(&data) {
            break;
        }
    }
    if let Some(h) = header.take() {
        write(&h);
    }
    child.wait().ok().and_then(|s| s.code())
}

/// mp4 のボックス (size, type, header_len) を読む
fn box_at(d: &[u8], off: usize) -> Option<(usize, [u8; 4], usize)> {
    let h = d.get(off..off + 8)?;
    let size32 = u32::from_be_bytes(h[0..4].try_into().unwrap()) as usize;
    let typ: [u8; 4] = h[4..8].try_into().unwrap();
    match size32 {
        0 => Some((d.len() - off, typ, 8)),
        1 => {
            let big = u64::from_be_bytes(d.get(off + 8..off + 16)?.try_into().unwrap()) as usize;
            Some((big, typ, 16))
        }
        s => Some((s, typ, 8)),
    }
}

/// 先頭の ftyp + moov が揃うまでのバイト数 (揃っていなければ None)
fn header_len(d: &[u8]) -> Option<usize> {
    let mut off = 0;
    loop {
        let (size, typ, _) = box_at(d, off)?;
        if off + size > d.len() {
            return None;
        }
        off += size;
        if &typ == b"moov" {
            return Some(off);
        }
    }
}

/// 断片化 mp4 の moov に全体の長さを書き込む (mvhd.duration を設定し、mvex に mehd を追加)。
/// これが無いとプレイヤーは長さを「不明 (無限)」と扱い、ライブ扱いになる。
fn patch_duration(header: &[u8], dur_secs: f64) -> Option<Vec<u8>> {
    let moov_off = {
        let mut off = 0;
        loop {
            let (size, typ, _) = box_at(header, off)?;
            if &typ == b"moov" {
                break off;
            }
            off += size;
        }
    };
    let (moov_size, _, moov_hl) = box_at(header, moov_off)?;
    let mut moov = header[moov_off..moov_off + moov_size].to_vec();

    // mvhd: timescale を読んで duration を書く
    let mut timescale = 0u32;
    let mut mvex_off = None;
    let mut off = moov_hl;
    while off < moov.len() {
        let (size, typ, hl) = box_at(&moov, off)?;
        let body = off + hl;
        match &typ {
            b"mvhd" => {
                let version = moov[body];
                if version == 1 {
                    timescale = u32::from_be_bytes(moov[body + 20..body + 24].try_into().ok()?);
                    let d = (dur_secs * timescale as f64) as u64;
                    moov[body + 24..body + 32].copy_from_slice(&d.to_be_bytes());
                } else {
                    timescale = u32::from_be_bytes(moov[body + 12..body + 16].try_into().ok()?);
                    let d = ((dur_secs * timescale as f64) as u64).min(u32::MAX as u64) as u32;
                    moov[body + 16..body + 20].copy_from_slice(&d.to_be_bytes());
                }
            }
            b"mvex" => mvex_off = Some((off, size, hl)),
            _ => {}
        }
        off += size;
    }
    if timescale == 0 {
        return None;
    }
    let (mvex_off, mvex_size, mvex_hl) = mvex_off?;
    if mvex_hl != 8 || moov_hl != 8 {
        return None;
    }
    // mehd (version 1): size 20, fragment_duration は mvhd の timescale 単位
    let mut mehd = Vec::with_capacity(20);
    mehd.extend_from_slice(&20u32.to_be_bytes());
    mehd.extend_from_slice(b"mehd");
    mehd.extend_from_slice(&[1, 0, 0, 0]);
    mehd.extend_from_slice(&((dur_secs * timescale as f64) as u64).to_be_bytes());
    // 既に mehd があれば追加しない
    let has_mehd = {
        let mut o = mvex_off + 8;
        let mut found = false;
        while o < mvex_off + mvex_size {
            let (s, t, _) = box_at(&moov, o)?;
            if &t == b"mehd" {
                found = true;
            }
            o += s;
        }
        found
    };
    if !has_mehd {
        moov.splice(mvex_off + 8..mvex_off + 8, mehd);
        let new_mvex = (mvex_size + 20) as u32;
        moov[mvex_off..mvex_off + 4].copy_from_slice(&new_mvex.to_be_bytes());
        let new_moov = (moov_size + 20) as u32;
        moov[0..4].copy_from_slice(&new_moov.to_be_bytes());
    }

    let mut out = header[..moov_off].to_vec();
    out.extend_from_slice(&moov);
    out.extend_from_slice(&header[moov_off + moov_size..]);
    Some(out)
}

fn remux(ffmpeg: &Path, src: &Path, dst: &Path, log: &Path) -> std::io::Result<u64> {
    let stderr = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let status = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "warning", "-y", "-i"])
        .arg(src)
        .args(["-map", "0", "-c", "copy", "-movflags", "+faststart", "-f", "mp4"])
        .arg(dst)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("ffmpeg exit {:?}", status.code())));
    }
    Ok(std::fs::metadata(dst)?.len())
}

/// "bytes=S-E" / "bytes=S-" を (S, Some(E)) にする
fn parse_range(r: &str) -> Option<(u64, Option<u64>)> {
    let spec = r.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (s, e) = first.split_once('-')?;
    let s: u64 = s.trim().parse().ok()?;
    let e = e.trim();
    Some((s, if e.is_empty() { None } else { e.parse().ok() }))
}

pub fn handle(stream: TcpStream, method: &str, range: Option<&str>, st: &BufState, content_type: &str, logger: &Logger, id: usize) -> std::io::Result<()> {
    if st.decide(logger) {
        let size = st.final_size();
        serve(stream, method, range, &st.final_path, size, &|| size, &|| true, false, true, content_type, logger, id)
    } else {
        let total = st.est_size.max(st.written());
        let tail = |start: u64| !st.done() && start >= total.saturating_sub(TAIL_PROBE) && start > st.written();
        let probe = range.and_then(parse_range).is_some_and(|(s, _)| tail(s));
        serve(stream, method, range, &st.path, total, &|| st.written(), &|| st.done(), probe, false, content_type, logger, id)
    }
}

/// 出来上がっているファイルを Range 対応で配信する
pub fn serve_file(stream: TcpStream, method: &str, range: Option<&str>, path: &Path, content_type: &str, logger: &Logger, id: usize) -> std::io::Result<()> {
    let size = std::fs::metadata(path)?.len();
    serve(stream, method, range, path, size, &|| size, &|| true, false, true, content_type, logger, id)
}

/// 実データの後ろ (推定サイズとの差分) の中身。
/// ゼロのままだとプレイヤーが壊れたボックスとみなして再生を止めるので、
/// 先頭に mp4 の `free` ボックスのヘッダを置いて「空き領域」として読ませる。
fn fill_padding(buf: &mut [u8], pos: u64, data_end: u64, total: u64) {
    buf.fill(0);
    let gap = total - data_end;
    let header: Vec<u8> = if gap >= 8 && gap <= u32::MAX as u64 {
        [&(gap as u32).to_be_bytes()[..], b"free"].concat()
    } else if gap >= 16 {
        [&1u32.to_be_bytes()[..], b"free", &gap.to_be_bytes()[..]].concat()
    } else {
        return; // 8 バイト未満は入れようがない (推定は常に十分大きいので通常起きない)
    };
    for (i, b) in header.iter().enumerate() {
        let abs = data_end + i as u64;
        if abs >= pos && abs < pos + buf.len() as u64 {
            buf[(abs - pos) as usize] = *b;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn serve(
    mut stream: TcpStream,
    method: &str,
    range: Option<&str>,
    path: &Path,
    total: u64,
    written: &dyn Fn() -> u64,
    done: &dyn Fn() -> bool,
    tail_probe: bool,
    // true なら実ファイルそのもの (パディング無し)
    exact: bool,
    content_type: &str,
    logger: &Logger,
    id: usize,
) -> std::io::Result<()> {
    let (start, end, partial) = match range.and_then(parse_range) {
        Some((s, e)) => (s, e.unwrap_or(total - 1).min(total - 1), true),
        None => (0, total - 1, false),
    };
    if start >= total {
        stream.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())?;
        logger.log(format!("conn#{id} -> 416 (start {start} >= total {total})"));
        return Ok(());
    }
    let len = end - start + 1;
    let mut head = if partial {
        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\n")
    } else {
        "HTTP/1.1 200 OK\r\n".to_string()
    };
    head.push_str(&format!("Content-Type: {content_type}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"));
    logger.log(format!(
        "conn#{id} serve {start}-{end}/{total} (have {:.1} MiB{})",
        written() as f64 / 1048576.0,
        if done() { ", done" } else { "" }
    ));
    stream.write_all(head.as_bytes())?;
    if method == "HEAD" {
        return Ok(());
    }

    let started = Instant::now();
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; CHUNK];
    let mut pos = start;
    let mut zeros_sent = 0u64;
    let mut last_progress = Instant::now();
    let result: std::io::Result<()> = (|| {
        while pos <= end {
            let want = ((end + 1 - pos) as usize).min(CHUNK);
            let avail = written();
            if pos < avail {
                let n = want.min((avail - pos) as usize);
                file.seek(SeekFrom::Start(pos))?;
                file.read_exact(&mut buf[..n])?;
                stream.write_all(&buf[..n])?;
                pos += n as u64;
                last_progress = Instant::now();
            } else if done() || tail_probe {
                if done() && !exact {
                    // 実データの後ろ (推定サイズとの差分) は free ボックス
                    fill_padding(&mut buf[..want], pos, written(), total);
                } else {
                    // ダウンロード中の末尾の覗き見 (mfra の確認など) にはゼロで答える
                    buf[..want].fill(0);
                }
                stream.write_all(&buf[..want])?;
                pos += want as u64;
                zeros_sent += want as u64;
            } else {
                if last_progress.elapsed() > STALL_TIMEOUT {
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no data from ffmpeg"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    })();
    logger.log(format!(
        "conn#{id} done: {} {:.1} MiB (zeros {:.1} MiB) in {:.1}s",
        match &result {
            Ok(()) => "sent".to_string(),
            Err(e) => format!("stopped ({e}) after"),
        },
        (pos - start) as f64 / 1048576.0,
        zeros_sent as f64 / 1048576.0,
        started.elapsed().as_secs_f64()
    ));
    Ok(())
}
