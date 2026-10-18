use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
pub struct Logger {
    path: PathBuf,
    tag: String,
}

impl Logger {
    pub fn new(dir: &Path, tag: impl Into<String>) -> Logger {
        let _ = std::fs::create_dir_all(dir);
        let name = format!("vrc-video-proxy_{}.log", chrono::Local::now().format("%Y%m%d"));
        Logger { path: dir.join(name), tag: tag.into() }
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let mut out = String::new();
        for line in msg.as_ref().lines() {
            out.push_str(&format!("{ts} [{}] {line}\n", self.tag));
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(out.as_bytes());
        }
    }
}

/// keep_days より古い *.log を消す
pub fn cleanup(dir: &Path, keep_days: u64) {
    if keep_days == 0 {
        return;
    }
    let Some(limit) = SystemTime::now().checked_sub(Duration::from_secs(keep_days * 86400)) else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "log") {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                if m < limit {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
}
