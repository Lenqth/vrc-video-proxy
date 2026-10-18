# vrc-video-proxy

[English](README.md)

VRChat の `Tools/yt-dlp.exe` の代わりに置くプロキシです。VRChat から渡された URL を、別の場所に置いた本物の yt-dlp で解決し、VRChat の動画プレイヤー (AVPro) で再生しやすい形にして返します。

## できること

- **自分の yt-dlp と引数を使う**: 最新の yt-dlp、cookies、追加引数をそのまま使えます
- **コーデック変換**: 再生できないコーデック (opus、VP9 など) や、映像と音声が別々の動画 (ニコニコなど) を ffmpeg で変換・結合して配信します
- **GPU エンコード**: 映像の変換が必要なとき、NVIDIA / AMD / Intel の GPU を自動で検出して使います (失敗したら CPU でやり直し)
- **シーク対応**: 変換中の動画も、取りながら配信して長さ・シークが使えます
- **IPv4 中継**: googlevideo への接続をプロキシが IPv4 で行い、IPv6 での 403 を避けます
- **ライブ対応**: VRChat の 1 回目の呼び出しでライブ配信を取れるようにし、失敗表示と待ち時間をなくします
- **キャッシュ**: プレイヤーのリロードなどで同じ URL が続けて来たときは即座に返します
- **ログ**: 受け取った引数、yt-dlp のエラー出力、選ばれた形式、変換の判断、AVPro からの通信を記録します
- **テスト動画** (オプトイン): 指定コーデックの小さな動画をローカルで生成して返します

## 必要なもの (各自でインストールしてください)

**yt-dlp と ffmpeg は同梱していません。** 利用者が各自でインストールしてください。

| ソフト | 用途 | 入手先 |
|---|---|---|
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | URL の解決 (必須) | [公式リリース](https://github.com/yt-dlp/yt-dlp/releases) (起動が速い `yt-dlp_win.zip` がおすすめ) / `winget install yt-dlp.yt-dlp` |
| [ffmpeg](https://ffmpeg.org) | 変換・結合・テスト動画 (推奨) | [ビルド一覧](https://ffmpeg.org/download.html#build-windows) / `winget install Gyan.FFmpeg` |

ffmpeg が無くても動きますが、変換が必要な動画 (ニコニコ、opus 音声など) は再生できません。

## インストール

配布パッケージ (zip) を展開して、PowerShell で実行します。

```powershell
.\install.ps1                              # インストール
.\install.ps1 -YtDlpPath C:\tools\yt-dlp.exe  # 本物の yt-dlp の場所を指定
.\install.ps1 -Uninstall                   # 削除 (VRChat が次回起動時に純正を入れ直す)
```

実行がブロックされる場合は `powershell -ExecutionPolicy Bypass -File .\install.ps1` を使ってください。

インストールスクリプトは次のことを行います。

- `yt-dlp.exe` (プロキシ本体) を Tools にコピーして読み取り専用にする (VRChat の起動時の上書き対策)
- 既存の `yt-dlp.exe` がリンクなら、リンクだけ消す (リンク先は変更しない)
- 使用中なら古いファイルを改名してから入れ替える
- `vrc-video-proxy.toml` を作り、本物の yt-dlp の場所を探して設定する (Tools\yt-dlp_win、ドキュメント、PATH)

> **注意**: Tools の `yt-dlp.exe` はリンクではなく、コピーで置いてください。
>
> - **ハードリンク**にすると、VRChat が起動時に `yt-dlp.exe` を上書きしたとき、元のファイル (ビルドした本体など) まで書き換わります。ハードリンクは別名ではなく同じファイルそのものだからです。
> - **シンボリックリンク**でも、VRChat に上書きされればプロキシが純正の yt-dlp に戻ってしまいます (リンク先まで書き換わるかは VRChat の書き換え方によります)。
>
> インストールスクリプトはコピーで置いて読み取り専用にするので、この問題は起きません。

## 設定

Tools の `vrc-video-proxy.toml` で設定します (初回起動時にコメント付きのテンプレートが作られます。パッケージの `vrc-video-proxy.example.toml` と同じ内容です)。主な項目:

| 項目 | 既定 | 内容 |
|---|---|---|
| `ytdlp_path` | | 本物の yt-dlp.exe の場所 |
| `args_mode` | `append` | `append`: VRChat の引数 + `extra_args` / `replace`: `extra_args` + URL のみ |
| `extra_args` | `[]` | yt-dlp に追加する引数 (`-4`、`-v` など) |
| `convert` | `auto` | `auto` / `always` / `never` |
| `allowed_video_codecs` | `["avc1", "h264"]` | 変換しなくてよい映像コーデック (前方一致) |
| `allowed_audio_codecs` | `["mp4a", "aac"]` | 変換しなくてよい音声コーデック (前方一致) |
| `prefer_allowed_codecs` | `true` | 形式を選ぶときに許容コーデックを優先する |
| `video_encoder` | `auto` | 映像変換のエンコーダ: `auto` / `nvenc` / `amf` / `qsv` / `cpu` |
| `video_transcode_args` | `[]` | 映像変換の ffmpeg 引数を直接指定 (指定すると `video_encoder` より優先) |
| `seekable_convert` | `true` | 変換した動画を保存しながら配信し、シークできるようにする |
| `relay` | `false` | 変換しない動画も中継する (IPv4 接続・403 リトライ) |
| `cache_ttl_secs` | `300` | 同じ引数の結果を使い回す秒数 |
| `live_fallback` | `true` | ライブ配信を 1 回目の呼び出しで取れるようにする |
| `prefer_audio` | `true` | 音声付きの形式を優先し、無ければ映像+音声を取って結合する |
| `test_videos` | `false` | テスト動画を有効にする |

## GPU エンコード

`video_encoder = "auto"` (既定) では、映像の変換が必要になったときに次の順で GPU エンコーダを**実際に試し**、使えたものを使います。

1. NVIDIA (`h264_nvenc`)
2. AMD Radeon (`h264_amf`)
3. Intel (`h264_qsv`)
4. どれも使えなければ CPU (`libx264`)

- 検出結果は `vrc-video-proxy-data\encoder.json` に保存し、7 日間または ffmpeg が変わるまで再検出しません
- GPU でのエンコードが失敗した場合は、自動で CPU でやり直します
- ビットレートは解像度ごとに上限を付けています (1080p で 8 Mbps など)
- 映像の変換が起きるのは、許容コーデック以外の映像しか無いときだけです (YouTube やニコニコは通常 H.264 のため変換しません)

## テスト動画

`test_videos = true` にすると、次の URL で指定コーデックの小さな動画を返します。AVPro がどのコーデックを再生できるかの確認に使えます。

```
https://vrc-video-proxy.test/hevc/opus
https://vrc-video-proxy.test/?vcodec=av1&acodec=opus&dur=30&res=1920x1080
https://www.youtube.com/watch?vrc_video_proxy_test&vcodec=vp9&raw=1
```

上の URL は一例です。`vcodec` / `acodec` の値 (パス形式なら `/hevc/opus` の部分) を下の表の値に変えれば、ほかのコーデックの組み合わせも流せます。

```
https://vrc-video-proxy.test/av1/aac          AV1 + AAC
https://vrc-video-proxy.test/vp8/vorbis       VP8 + Vorbis (自動で webm)
https://vrc-video-proxy.test/h264/none        映像のみ (音声なし)
https://vrc-video-proxy.test/none/flac        音声のみ
```

| パラメータ | 値 |
|---|---|
| `vcodec` | `h264` `hevc` `av1` `vp9` `vp8` `none` (または ffmpeg のエンコーダ名) |
| `acodec` | `aac` `opus` `mp3` `vorbis` `flac` `none` |
| `dur` | 秒 (既定 10、最大 600) |
| `res` / `fps` | 既定 `1280x720` / `30` |
| `container` | `mp4` `webm` `mkv` (vp8 / vorbis は既定で webm) |
| `raw` | `1` で許容コーデック判定・変換をせずにそのまま返す |

- `raw` なし: 実際の動画と同じ判定を通ります (許容コーデック以外は変換)
- VRChat は信頼済みドメイン以外の URL を弾くことがあります。その場合は YouTube の URL に `vrc_video_proxy_test` を付ける形を使ってください
- 生成した動画は `vrc-video-proxy-data\test\` に保存され、同じ指定なら使い回します

## ログ

- `vrc-video-proxy-logs\vrc-video-proxy_YYYYMMDD.log`: メインのログ (各行に PID、配信サーバ側は `serve <pid>`)
- `vrc-video-proxy-logs\ffmpeg_*.log`: ffmpeg のエラー出力
- yt-dlp の詳しいログが必要なら `extra_args` に `"-v"` を追加

## ソースからビルド

```powershell
cargo build --release      # target\release\yt-dlp.exe
.\install.ps1              # ビルドしてインストール (-NoBuild でビルドを省略)
.\make-dist.ps1            # 配布パッケージを dist\ に作成
```

## 注意事項

- 本ソフトは非公式のツールです。VRChat、YouTube、ニコニコ動画、yt-dlp、ffmpeg の各開発元・運営元とは関係ありません。
- 利用は自己責任でお願いします。本ソフトの使用によって生じたいかなる損害についても、作者は責任を負いません。
- 動画の取得・再生にあたっては、各動画サービスの利用規約と著作権を守ってください。
- VRChat の Tools フォルダのファイルを置き換えることが VRChat の利用規約上どう扱われるかは、各自で確認してください。
- VRChat や yt-dlp、各動画サービスの仕様変更により、動作しなくなることがあります。

## ライセンス

Copyright (c) 2026 Lenqth

[MIT License](LICENSE-MIT) または [Apache License 2.0](LICENSE-APACHE) のどちらかを選んで利用できます (デュアルライセンス)。

本体に含まれる依存クレートのライセンスは、配布パッケージの `THIRD_PARTY_LICENSES.txt` にまとめています。yt-dlp と ffmpeg は同梱せず、別プロセスとして呼び出すだけです (それぞれのライセンスは各配布元を参照してください)。
