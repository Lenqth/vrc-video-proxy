# vrc-video-proxy

[日本語](README_ja.md)

A drop-in replacement for VRChat's `Tools/yt-dlp.exe`. It resolves the URLs VRChat passes in using your own copy of yt-dlp, and returns them in a form that VRChat's video player (AVPro) can play reliably.

## Features

- **Use your own yt-dlp and arguments**: latest yt-dlp, cookies, extra arguments
- **Codec conversion**: converts / merges with ffmpeg when the codec is not playable (Opus, VP9, ...) or when video and audio are separate streams (e.g. Niconico)
- **GPU encoding**: when video has to be re-encoded, NVIDIA / AMD / Intel GPUs are detected and used automatically (falls back to CPU on failure)
- **Seeking**: converted videos are served while downloading, with duration and seeking available
- **IPv4 relay**: the proxy connects to googlevideo over IPv4 to avoid HTTP 403 on IPv6
- **Live streams**: resolved on VRChat's first attempt, avoiding the error flash and extra wait
- **Cache**: repeated requests for the same URL (e.g. player reloads) are answered instantly
- **Logs**: arguments received, yt-dlp's stderr, selected formats, conversion decisions and AVPro's requests
- **Test videos** (opt-in): small locally generated videos with the codecs you specify

## Requirements (install them yourself)

**yt-dlp and ffmpeg are NOT included.** Please install them yourself.

| Software | Used for | Get it from |
|---|---|---|
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | resolving URLs (required) | [official releases](https://github.com/yt-dlp/yt-dlp/releases) (`yt-dlp_win.zip` starts faster) / `winget install yt-dlp.yt-dlp` |
| [ffmpeg](https://ffmpeg.org) | conversion, merging, test videos (recommended) | [builds](https://ffmpeg.org/download.html#build-windows) / `winget install Gyan.FFmpeg` |

Without ffmpeg the proxy still works, but videos that need conversion (Niconico, Opus audio, ...) cannot be played.

## Install

Extract the release zip and run in PowerShell:

```powershell
.\install.ps1                                 # install
.\install.ps1 -YtDlpPath C:\tools\yt-dlp.exe  # specify the real yt-dlp
.\install.ps1 -Uninstall                      # remove (VRChat reinstalls its own on next start)
```

If script execution is blocked, use `powershell -ExecutionPolicy Bypass -File .\install.ps1`.

The install script:

- copies `yt-dlp.exe` (the proxy) into Tools and makes it read-only (so VRChat does not overwrite it on startup)
- removes only the link if the existing `yt-dlp.exe` is a symbolic / hard link (the link target is untouched)
- renames the old file first if it is in use
- creates `vrc-video-proxy.toml` and looks for the real yt-dlp (Tools\yt-dlp_win, Documents, PATH)

> **Note**: place Tools\yt-dlp.exe as a copy, not a link.
>
> - With a **hard link**, when VRChat overwrites `yt-dlp.exe` on startup, the original file (e.g. your build output) is overwritten too, because a hard link is the same file, not an alias.
> - With a **symbolic link**, the proxy is still replaced by the stock yt-dlp when VRChat overwrites it (whether the link target is also overwritten depends on how VRChat writes the file).
>
> The install script places a read-only copy, so this does not happen.

## Configuration

Edit `vrc-video-proxy.toml` in the Tools folder (a commented template is created on first run; same as `vrc-video-proxy.example.toml` in the package). Main options:

| Option | Default | Description |
|---|---|---|
| `ytdlp_path` | | path of the real yt-dlp.exe |
| `args_mode` | `append` | `append`: VRChat's args + `extra_args` / `replace`: `extra_args` + URL only |
| `extra_args` | `[]` | extra arguments for yt-dlp (`-4`, `-v`, ...) |
| `convert` | `auto` | `auto` / `always` / `never` |
| `allowed_video_codecs` | `["avc1", "h264"]` | video codecs that need no conversion (prefix match) |
| `allowed_audio_codecs` | `["mp4a", "aac"]` | audio codecs that need no conversion (prefix match) |
| `prefer_allowed_codecs` | `true` | prefer formats with allowed codecs |
| `video_encoder` | `auto` | encoder for video conversion: `auto` / `nvenc` / `amf` / `qsv` / `cpu` |
| `video_transcode_args` | `[]` | custom ffmpeg arguments for video conversion (overrides `video_encoder`) |
| `seekable_convert` | `true` | serve converted videos while saving them, so seeking works |
| `relay` | `false` | relay unconverted videos too (IPv4, retry on 403) |
| `cache_ttl_secs` | `300` | reuse results for identical arguments for this many seconds |
| `live_fallback` | `true` | resolve live streams on the first attempt |
| `prefer_audio` | `true` | prefer formats with audio; otherwise fetch video + audio and merge |
| `test_videos` | `false` | enable test videos |

## GPU encoding

With `video_encoder = "auto"` (default), when video conversion is needed the proxy **actually tries** GPU encoders in this order and uses the first one that works:

1. NVIDIA (`h264_nvenc`)
2. AMD Radeon (`h264_amf`)
3. Intel (`h264_qsv`)
4. CPU (`libx264`) if none is available

- The result is saved in `vrc-video-proxy-data\encoder.json` and re-checked after 7 days or when ffmpeg changes
- If GPU encoding fails, it is retried on the CPU automatically
- Bitrate is capped per resolution (e.g. 8 Mbps for 1080p)
- Video is only re-encoded when no format with an allowed video codec exists (YouTube and Niconico normally provide H.264, so no re-encoding happens)

## Test videos

With `test_videos = true`, these URLs return a small video with the requested codecs — useful for checking what AVPro can play.

```
https://vrc-video-proxy.test/hevc/opus
https://vrc-video-proxy.test/?vcodec=av1&acodec=opus&dur=30&res=1920x1080
https://www.youtube.com/watch?vrc_video_proxy_test&vcodec=vp9&raw=1
```

These are just examples. Change the `vcodec` / `acodec` values (or the `/hevc/opus` part in the path form) to any value in the table below to stream other codec combinations:

```
https://vrc-video-proxy.test/av1/aac          AV1 + AAC
https://vrc-video-proxy.test/vp8/vorbis       VP8 + Vorbis (webm automatically)
https://vrc-video-proxy.test/h264/none        video only (no audio)
https://vrc-video-proxy.test/none/flac        audio only
```

| Parameter | Values |
|---|---|
| `vcodec` | `h264` `hevc` `av1` `vp9` `vp8` `none` (or an ffmpeg encoder name) |
| `acodec` | `aac` `opus` `mp3` `vorbis` `flac` `none` |
| `dur` | seconds (default 10, max 600) |
| `res` / `fps` | default `1280x720` / `30` |
| `container` | `mp4` `webm` `mkv` (vp8 / vorbis default to webm) |
| `raw` | `1` to return as-is, skipping the codec check and conversion |

- Without `raw`, the video goes through the same decisions as real videos (non-allowed codecs are converted)
- VRChat may block URLs outside its trusted domains; in that case add `vrc_video_proxy_test` to a YouTube URL
- Generated videos are stored in `vrc-video-proxy-data\test\` and reused for identical parameters

## Logs

- `vrc-video-proxy-logs\vrc-video-proxy_YYYYMMDD.log`: main log (each line has the PID; server lines are tagged `serve <pid>`)
- `vrc-video-proxy-logs\ffmpeg_*.log`: ffmpeg's stderr
- add `"-v"` to `extra_args` for verbose yt-dlp logs

## Building from source

```powershell
cargo build --release      # target\release\yt-dlp.exe
.\install.ps1              # build and install (-NoBuild to skip building)
.\make-dist.ps1            # create the release package in dist\
```

## Disclaimer

- This is an unofficial tool. It is not affiliated with VRChat, YouTube, Niconico, yt-dlp or ffmpeg.
- Use at your own risk. The author is not responsible for any damage caused by using this software.
- Respect the terms of service of each video service and copyright when fetching and playing videos.
- Check for yourself how replacing files in VRChat's Tools folder is treated under VRChat's terms of service.
- It may stop working due to changes in VRChat, yt-dlp or the video services.

## License

Copyright (c) 2026 Lenqth

Licensed under either of [MIT License](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Licenses of the dependencies compiled into the binary are collected in `THIRD_PARTY_LICENSES.txt` in the release package. yt-dlp and ffmpeg are not bundled; they are only invoked as separate processes (see their distributors for their licenses).
