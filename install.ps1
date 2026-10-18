<#
.SYNOPSIS
    Install / uninstall vrc-video-proxy into VRChat's Tools folder.
    vrc-video-proxy を VRChat の Tools フォルダにインストール / アンインストールする。

.DESCRIPTION
    - Copies yt-dlp.exe (next to this script in a release package, or target\release\yt-dlp.exe
      in the source tree, built if needed) to Tools\yt-dlp.exe and makes it read-only
      so that VRChat does not overwrite it on startup.
    - If the existing Tools\yt-dlp.exe is a symbolic / hard link, only the link is removed.
    - If the file is in use (relay server running), it is renamed before being replaced.
    - Creates vrc-video-proxy.toml if missing and sets the path of the real yt-dlp.

    yt-dlp and ffmpeg are NOT included. Install them yourself.
    yt-dlp と ffmpeg は同梱していません。各自でインストールしてください。

.PARAMETER ToolsDir
    Install destination. Default: %USERPROFILE%\AppData\LocalLow\VRChat\VRChat\Tools

.PARAMETER YtDlpPath
    Path of the real yt-dlp.exe to write into the config.

.PARAMETER NoBuild
    Source tree only: do not build, use the existing target\release\yt-dlp.exe.

.PARAMETER Uninstall
    Remove the proxy (VRChat reinstalls its own yt-dlp.exe on next start). Config and logs are kept.

.EXAMPLE
    .\install.ps1
    .\install.ps1 -YtDlpPath C:\tools\yt-dlp.exe
    .\install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$ToolsDir = (Join-Path $env:USERPROFILE "AppData\LocalLow\VRChat\VRChat\Tools"),
    [string]$YtDlpPath,
    [switch]$NoBuild,
    [switch]$Uninstall
)
$ErrorActionPreference = "Stop"

# 表示言語か地域設定のどちらかが日本語なら日本語で表示する
$ja = ((Get-UICulture).Name -like "ja*") -or ((Get-Culture).Name -like "ja*")
function M([string]$jaText, [string]$enText) { if ($ja) { $jaText } else { $enText } }
function Info($m) { Write-Host "[*] $m" }
function Ok($m) { Write-Host "[+] $m" -ForegroundColor Green }
function Warn($m) { Write-Host "[!] $m" -ForegroundColor Yellow }

$target = Join-Path $ToolsDir "yt-dlp.exe"
$config = Join-Path $ToolsDir "vrc-video-proxy.toml"

function Test-IsProxy($path) {
    if (-not (Test-Path $path)) { return $false }
    try { return ((& $path --vrc-video-proxy-version 2>$null) -join "") -like "vrc-video-proxy*" } catch { return $false }
}

# Move the existing yt-dlp.exe out of the way. Remove only the link if it is a link; rename if in use.
function Remove-Existing {
    if (-not (Test-Path $target)) { return }
    $item = Get-Item $target -Force
    if ($item.LinkType) {
        Info (M "既存の yt-dlp.exe は $($item.LinkType) です。リンクだけ削除します (リンク先は変更しません)" `
                "Existing yt-dlp.exe is a $($item.LinkType). Removing the link only (the target is not touched)")
        Remove-Item $target -Force
        return
    }
    if ($item.IsReadOnly) { $item.IsReadOnly = $false }
    try {
        Remove-Item $target -Force
    } catch {
        $old = "$target.old" + (Get-Date -Format "yyyyMMddHHmmss")
        Info (M "yt-dlp.exe が使用中のため $([IO.Path]::GetFileName($old)) に改名します" `
                "yt-dlp.exe is in use, renaming it to $([IO.Path]::GetFileName($old))")
        Rename-Item $target $old
    }
}

function Remove-OldFiles {
    Get-ChildItem $ToolsDir -Filter "yt-dlp.exe.old*" -ErrorAction SilentlyContinue | ForEach-Object {
        try { Remove-Item $_.FullName -Force } catch { }
    }
}

if (-not (Test-Path $ToolsDir)) {
    throw (M "Tools フォルダが見つかりません: $ToolsDir (VRChat を一度起動してから実行してください)" `
             "Tools folder not found: $ToolsDir (start VRChat once, then run this again)")
}

# ---------------- Uninstall ----------------
if ($Uninstall) {
    if (-not (Test-IsProxy $target)) {
        Warn (M "Tools\yt-dlp.exe はこのプロキシではないため、何もしません" "Tools\yt-dlp.exe is not this proxy; nothing to do")
        exit 0
    }
    Remove-Existing
    Remove-OldFiles
    Ok (M "プロキシを削除しました。VRChat の次回起動時に純正の yt-dlp.exe が入ります" `
          "Proxy removed. VRChat will reinstall its own yt-dlp.exe on next start")
    Info (M "設定 ($config) とログ・データフォルダは残しています。不要なら手動で削除してください" `
            "Config ($config), logs and data folders are kept. Delete them manually if not needed")
    exit 0
}

# ---------------- Source binary ----------------
$isSourceTree = Test-Path (Join-Path $PSScriptRoot "Cargo.toml")
if ($isSourceTree) {
    $src = Join-Path $PSScriptRoot "target\release\yt-dlp.exe"
    if (-not $NoBuild) {
        if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
            throw (M "cargo が見つかりません。Rust (https://rustup.rs) をインストールするか、-NoBuild を指定してください" `
                     "cargo not found. Install Rust (https://rustup.rs) or use -NoBuild")
        }
        Info (M "ビルドしています (cargo build --release)" "Building (cargo build --release)")
        Push-Location $PSScriptRoot
        try {
            cargo build --release
            if ($LASTEXITCODE -ne 0) { throw (M "ビルドに失敗しました" "Build failed") }
        } finally { Pop-Location }
    }
} else {
    # Release package: yt-dlp.exe is next to this script
    $src = Join-Path $PSScriptRoot "yt-dlp.exe"
}
if (-not (Test-IsProxy $src)) {
    throw (M "プロキシ本体が見つからないか、プロキシではありません: $src" "Proxy binary not found or not the proxy: $src")
}

# ---------------- Install ----------------
Remove-Existing
Copy-Item $src $target
Set-ItemProperty $target -Name IsReadOnly -Value $true
Remove-OldFiles
Ok ((M "インストールしました" "Installed") + ": $target ($((& $target --vrc-video-proxy-version) -join ''))")

# ---------------- Real yt-dlp (installed by the user) ----------------
# Config (the proxy writes a template itself when missing)
if (-not (Test-Path $config)) {
    & $target --vrc-video-proxy-version | Out-Null
    Info ((M "設定ファイルを作成しました" "Created config") + ": $config")
}
$text = Get-Content $config -Raw -Encoding UTF8
$current = [regex]::Match($text, "(?m)^ytdlp_path\s*=\s*'([^']*)'").Groups[1].Value

if (-not $YtDlpPath -and ($current -eq "" -or $current -eq 'C:\path\to\yt-dlp.exe' -or -not (Test-Path $current))) {
    # Not configured: look in common places
    $candidates = @(
        (Join-Path $ToolsDir "yt-dlp_win\yt-dlp.exe"),
        (Join-Path $env:USERPROFILE "Documents\yt-dlp.exe")
    )
    $onPath = Get-Command yt-dlp.exe -All -ErrorAction SilentlyContinue | Where-Object { $_.Source -ne $target } | Select-Object -First 1
    if ($onPath) { $candidates += $onPath.Source }
    $YtDlpPath = $candidates | Where-Object { (Test-Path $_) -and -not (Test-IsProxy $_) } | Select-Object -First 1
}
if ($YtDlpPath) {
    $YtDlpPath = (Resolve-Path $YtDlpPath).Path
    if (Test-IsProxy $YtDlpPath) { throw (M "指定された yt-dlp はこのプロキシ自身です: $YtDlpPath" "The given yt-dlp is the proxy itself: $YtDlpPath") }
    $text = [regex]::Replace($text, "(?m)^ytdlp_path\s*=.*$", "ytdlp_path = '$YtDlpPath'")
    [IO.File]::WriteAllText($config, $text, (New-Object Text.UTF8Encoding $false))
    Ok ("yt-dlp: $YtDlpPath ($((& $YtDlpPath --version) -join ''))")
} elseif ($current -and (Test-Path $current)) {
    Ok "yt-dlp: $current"
} else {
    Warn (M "yt-dlp が見つかりません。yt-dlp を各自でインストールし、-YtDlpPath を指定するか $config の ytdlp_path を設定してください (https://github.com/yt-dlp/yt-dlp)" `
            "yt-dlp not found. Install yt-dlp yourself and pass -YtDlpPath, or set ytdlp_path in $config (https://github.com/yt-dlp/yt-dlp)")
}

# ---------------- ffmpeg ----------------
$ffmpegSetting = [regex]::Match($text, '(?m)^ffmpeg_path\s*=\s*"([^"]*)"').Groups[1].Value
if (-not $ffmpegSetting) { $ffmpegSetting = "ffmpeg" }
if (Get-Command $ffmpegSetting -ErrorAction SilentlyContinue) {
    Ok "ffmpeg: $((Get-Command $ffmpegSetting).Source)"
} else {
    Warn (M "ffmpeg が見つかりません ($ffmpegSetting)。コーデック変換・結合・テスト動画には ffmpeg が必要です。各自でインストールしてください (例: winget install Gyan.FFmpeg)" `
            "ffmpeg not found ($ffmpegSetting). It is required for conversion, merging and test videos. Install it yourself (e.g. winget install Gyan.FFmpeg)")
}

Info ((M "ログ" "Logs") + ": $(Join-Path $ToolsDir 'vrc-video-proxy-logs')")
Info (M "非公式ツールです。利用は自己責任で、各動画サービスの利用規約と著作権を守ってください (詳細は README の「注意事項」)" `
        "This is an unofficial tool. Use at your own risk and respect each service's terms and copyright (see Disclaimer in README)")
