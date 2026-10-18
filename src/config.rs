use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const CONFIG_NAME: &str = "vrc-video-proxy.toml";

const DEFAULT_CONFIG: &str = r#"# vrc-video-proxy 設定ファイル
# 相対パスはこのファイル(= yt-dlp.exe)のあるフォルダ基準

# 本物の yt-dlp.exe の場所 (必須)。例: ytdlp_path = 'C:\tools\yt-dlp_win\yt-dlp.exe'
ytdlp_path = ''

# "append" : VRChat が渡した引数 + extra_args   (後ろの方が優先されるので -f 等は上書きできる)
# "replace": extra_args + URL だけ (VRChat の引数は捨てる)
args_mode = "append"

# yt-dlp に追加で渡す引数
extra_args = [
    # "-f", "bv*[height<=1080][vcodec^=avc1]+ba/b[height<=1080]",
    # "-v",
]

# yt-dlp / ffmpeg の作業ディレクトリ (空 = このフォルダ)
# ※ yt-dlp はカレントの yt-dlp.conf も読むので、ここに置いた yt-dlp.conf は引き続き有効
working_dir = ""

# ログ出力先 (空 = ./vrc-video-proxy-logs)
log_dir = ""
# これより古いログは起動時に削除 (0 = 削除しない)
log_keep_days = 7

# "auto"  : 音声が AAC 以外、または映像/音声が別URLのときだけ ffmpeg で変換
# "always": 常に ffmpeg を通す
# "never" : 変換しない (yt-dlp の --get-url 結果をそのまま返す)
convert = "auto"

ffmpeg_path = "ffmpeg"

# 再生できる (変換しなくてよい) コーデック。前方一致・大文字小文字無視、空にすると全部許容
#   映像の例: "avc1" (H.264), "hev1"/"hvc1" (H.265), "vp09", "av01"
#   音声の例: "mp4a" (AAC), "opus", "mp3"
allowed_video_codecs = ["avc1", "h264"]
allowed_audio_codecs = ["mp4a", "aac"]
# 形式を選ぶときに許容コーデックのものを優先する (変換が要らなくなる)
prefer_allowed_codecs = true
# 映像が許容コーデックでないときに H.264 へ変換するエンコーダ
#   "auto" : GPU (NVIDIA → AMD → Intel) を実際に試して使えるものを使う。無ければ CPU
#   "nvenc" / "amf" / "qsv" / "cpu" : 固定
# GPU で失敗したときは自動で CPU でやり直す
video_encoder = "auto"
# 変換の ffmpeg 引数を直接指定する場合 (指定すると video_encoder より優先)
video_transcode_args = []
# 変換する動画 (ライブ以外) を、保存しながら流してシークできるようにする
seekable_convert = true
# 0 なら取りながらすぐ配信する。
# 1 以上なら、最初のリクエストで取り終わるのをこの秒数まで待ち、間に合えば普通の mp4 にして配信する
buffer_wait_secs = 0

# 同じ引数での解決結果をこの秒数だけ使い回す (0 = 無効)
# プレイヤーが読み込み待ちでリロードしてきたときに即応答するため
cache_ttl_secs = 300

# VRChat の -f に付く [protocol^=http] で失敗するライブ配信を、1 回目の呼び出しで取れるようにする
live_fallback = true
# 音声付きの形式を優先し、無ければ映像+音声を別々に取って ffmpeg で結合する (ニコニコ等)
prefer_audio = true

# 変換しない動画も、ローカルサーバ経由で中継する
# (VRChat の通信に関係なく、googlevideo へは中継側が接続する。403 は自動リトライ)
relay = false
# 中継時に IPv4 だけで接続する
relay_ipv4 = true
# 403 / 5xx / 通信エラー時のリトライ回数
relay_retries = 5
# 中継サーバは一時停止中も残しておきたいので長め
relay_idle_timeout_secs = 3600

# テスト動画 (オプトイン)。有効にすると、次の URL でローカル生成した指定コーデックの小さな動画を返す
#   https://vrc-video-proxy.test/?vcodec=hevc&acodec=opus      (パス形式: https://vrc-video-proxy.test/hevc/opus)
#   https://www.youtube.com/watch?vrc_video_proxy_test&vcodec=vp9   (信頼済みドメインに目印を付ける形)
# パラメータ: vcodec = h264/hevc/av1/vp9/vp8/none (または ffmpeg のエンコーダ名)
#             acodec = aac/opus/mp3/vorbis/flac/none, dur = 秒 (既定 10), res = 1280x720, fps = 30,
#             container = mp4/webm/mkv, raw = 1 (許容コーデック判定・変換をしない)
test_videos = false
test_host = "vrc-video-proxy.test"

# ローカル配信サーバ
listen_host = "127.0.0.1"
# 接続が無い状態がこの秒数続いたらサーバ終了
idle_timeout_secs = 120

# 各 -i の前に付ける入力オプション
ffmpeg_input_args = ["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "5"]
# 映像が許容コーデックのときの設定 (コピー)
ffmpeg_video_args = ["-c:v", "copy"]
# 音声が許容コーデックでないときのエンコード設定 (許容なら -c:a copy)
ffmpeg_audio_args = ["-c:a", "aac", "-b:a", "192k", "-ac", "2"]
# 出力形式 (stdout に流すのでシーク不可の形式にする)
ffmpeg_output_args = ["-f", "mp4", "-movflags", "frag_keyframe+empty_moov+default_base_moof"]
content_type = "video/mp4"
# MPEG-TS にしたい場合:
# ffmpeg_output_args = ["-f", "mpegts"]
# content_type = "video/mp2t"
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgsMode {
    Append,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConvertMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ytdlp_path: String,
    pub args_mode: ArgsMode,
    pub extra_args: Vec<String>,
    pub working_dir: String,
    pub log_dir: String,
    pub log_keep_days: u64,
    pub convert: ConvertMode,
    pub ffmpeg_path: String,
    pub allowed_video_codecs: Vec<String>,
    pub allowed_audio_codecs: Vec<String>,
    pub prefer_allowed_codecs: bool,
    pub video_encoder: String,
    pub video_transcode_args: Vec<String>,
    pub seekable_convert: bool,
    pub buffer_wait_secs: u64,
    pub cache_ttl_secs: u64,
    pub live_fallback: bool,
    pub prefer_audio: bool,
    pub relay: bool,
    pub relay_ipv4: bool,
    pub relay_retries: u32,
    pub relay_idle_timeout_secs: u64,
    pub test_videos: bool,
    pub test_host: String,
    pub listen_host: String,
    pub idle_timeout_secs: u64,
    pub ffmpeg_input_args: Vec<String>,
    pub ffmpeg_video_args: Vec<String>,
    pub ffmpeg_audio_args: Vec<String>,
    pub ffmpeg_output_args: Vec<String>,
    pub content_type: String,
}

impl Default for Config {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        Config {
            ytdlp_path: String::new(),
            args_mode: ArgsMode::Append,
            extra_args: vec![],
            working_dir: String::new(),
            log_dir: String::new(),
            log_keep_days: 7,
            convert: ConvertMode::Auto,
            ffmpeg_path: "ffmpeg".into(),
            allowed_video_codecs: s(&["avc1", "h264"]),
            allowed_audio_codecs: s(&["mp4a", "aac"]),
            prefer_allowed_codecs: true,
            video_encoder: "auto".into(),
            video_transcode_args: vec![],
            seekable_convert: true,
            buffer_wait_secs: 0,
            cache_ttl_secs: 300,
            live_fallback: true,
            prefer_audio: true,
            relay: false,
            relay_ipv4: true,
            relay_retries: 5,
            relay_idle_timeout_secs: 3600,
            test_videos: false,
            test_host: "vrc-video-proxy.test".into(),
            listen_host: "127.0.0.1".into(),
            idle_timeout_secs: 120,
            ffmpeg_input_args: s(&["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "5"]),
            ffmpeg_video_args: s(&["-c:v", "copy"]),
            ffmpeg_audio_args: s(&["-c:a", "aac", "-b:a", "192k", "-ac", "2"]),
            ffmpeg_output_args: s(&["-f", "mp4", "-movflags", "frag_keyframe+empty_moov+default_base_moof"]),
            content_type: "video/mp4".into(),
        }
    }
}

impl Config {
    /// 設定を読む。無ければテンプレートを書き出してデフォルトで続行。
    /// 戻り値の String は読み込み時の警告 (ログ用)。
    pub fn load(base: &Path) -> (Config, Option<String>) {
        let path = base.join(CONFIG_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("config parse error ({}): {e}", path.display()))),
            },
            Err(_) => {
                let _ = std::fs::write(&path, DEFAULT_CONFIG);
                (Config::default(), Some(format!("config not found, template written to {}", path.display())))
            }
        }
    }

    pub fn resolve(base: &Path, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        }
    }

    /// 実行ファイル名だけ ("ffmpeg" など) の場合は PATH 検索に任せる
    pub fn resolve_exe(base: &Path, p: &str) -> PathBuf {
        if p.contains('\\') || p.contains('/') {
            Self::resolve(base, p)
        } else {
            PathBuf::from(p)
        }
    }
}
