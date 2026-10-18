<#
.SYNOPSIS
    配布用パッケージ (dist\vrc-video-proxy-<version>-windows-x64\ と .zip) を作る。

.DESCRIPTION
    含めるもの:
      yt-dlp.exe                  プロキシ本体 (release ビルド)
      install.ps1                 インストールスクリプト
      README.md / README_ja.md
      LICENSE*                    このソフトのライセンス (リポジトリ直下にあるもの)
      THIRD_PARTY_LICENSES.txt    本体に静的リンクされる依存クレートのライセンス
      vrc-video-proxy.example.toml   設定ファイルのテンプレート
    yt-dlp と ffmpeg は含めない (利用者が各自でインストールする)。

.PARAMETER NoBuild
    ビルドせず既存の target\release\yt-dlp.exe を使う

.PARAMETER NoZip
    zip を作らない
#>
[CmdletBinding()]
param(
    [switch]$NoBuild,
    [switch]$NoZip
)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
Push-Location $root
try {
    $meta = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $pkg = $meta.packages | Where-Object { $_.name -eq "vrc-video-proxy" }
    $version = $pkg.version
    $name = "vrc-video-proxy-$version-windows-x64"
    $out = Join-Path $root "dist\$name"

    # 配布用ビルド: exe に埋め込まれるソースのパス (エラーメッセージ用) から
    # ユーザー名などの個人情報が入らないように、パスを置き換える。
    # 通常のビルドと設定が違うので target\dist に分けてビルドする。
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE ".rustup" }
    $remap = @("--remap-path-prefix=$cargoHome=cargo", "--remap-path-prefix=$rustupHome=rustup", "--remap-path-prefix=$root=vrc-video-proxy")
    $targetDir = Join-Path $root "target\dist"
    if (-not $NoBuild) {
        Write-Host "[*] cargo build --release (dist)"
        $saved = $env:CARGO_ENCODED_RUSTFLAGS
        # スペースを含むパスでも壊れないよう、区切りに 0x1f を使う形式で渡す
        $env:CARGO_ENCODED_RUSTFLAGS = $remap -join [char]0x1f
        try {
            cargo build --release --target-dir $targetDir
            if ($LASTEXITCODE -ne 0) { throw "build failed" }
        } finally { $env:CARGO_ENCODED_RUSTFLAGS = $saved }
    }
    $exe = Join-Path $targetDir "release\yt-dlp.exe"
    if (-not (Test-Path $exe)) { throw "not found: $exe (run without -NoBuild first)" }

    # 個人情報が残っていないか確認
    $bin = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($exe))
    foreach ($needle in @($env:USERNAME, $env:USERPROFILE) | Where-Object { $_ }) {
        if ($bin.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0) { throw "exe contains personal path/name: $needle" }
    }
    Write-Host "[+] no user name / profile path in exe"

    if (Test-Path $out) { Remove-Item $out -Recurse -Force }
    New-Item -ItemType Directory -Force $out | Out-Null

    # ---- 本体・スクリプト・ドキュメント ----
    Copy-Item $exe $out
    foreach ($f in "install.ps1", "README.md", "README_ja.md") {
        if (Test-Path $f) { Copy-Item $f $out } else { Write-Warning "missing: $f" }
    }
    $licenses = Get-ChildItem $root -File | Where-Object { $_.Name -match '^(LICENSE|COPYING)' }
    if ($licenses) { $licenses | Copy-Item -Destination $out }
    else { Write-Warning "LICENSE ファイルがありません (リポジトリ直下に LICENSE* を置いてください)" }

    # ---- 設定テンプレート (本体に一時フォルダで書き出させる) ----
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("vrc-video-proxy-dist-" + [guid]::NewGuid())
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        Copy-Item $exe $tmp
        & (Join-Path $tmp "yt-dlp.exe") --vrc-video-proxy-version | Out-Null
        Copy-Item (Join-Path $tmp "vrc-video-proxy.toml") (Join-Path $out "vrc-video-proxy.example.toml")
    } finally { Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue }

    # ---- 依存クレートのライセンス ----
    Write-Host "[*] collecting third-party licenses"
    $full = cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc | ConvertFrom-Json
    $byId = @{}
    foreach ($p in $full.packages) { $byId[$p.id] = $p }
    # 実際にビルドに使われる (normal 依存の) パッケージをたどる
    $nodes = @{}
    foreach ($n in $full.resolve.nodes) { $nodes[$n.id] = $n }
    $rootId = ($full.packages | Where-Object { $_.name -eq "vrc-video-proxy" }).id
    $seen = @{}
    $stack = New-Object System.Collections.Stack
    $stack.Push($rootId)
    while ($stack.Count -gt 0) {
        $id = $stack.Pop()
        foreach ($d in $nodes[$id].deps) {
            $normal = $d.dep_kinds | Where-Object { $null -eq $_.kind -or $_.kind -eq "normal" }
            if ($normal -and -not $seen.ContainsKey($d.pkg)) { $seen[$d.pkg] = $true; $stack.Push($d.pkg) }
        }
    }
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.AppendLine("Third-party licenses for vrc-video-proxy $version")
    [void]$sb.AppendLine("vrc-video-proxy の本体に含まれる依存クレートのライセンス")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("yt-dlp and ffmpeg are not included; they are installed separately by the user.")
    [void]$sb.AppendLine("")
    $deps = $seen.Keys | ForEach-Object { $byId[$_] } | Sort-Object name, version
    [void]$sb.AppendLine("Summary:")
    foreach ($p in $deps) { [void]$sb.AppendLine(("  {0} {1}  ({2})" -f $p.name, $p.version, $p.license)) }
    foreach ($p in $deps) {
        [void]$sb.AppendLine("")
        [void]$sb.AppendLine(("=" * 78))
        [void]$sb.AppendLine(("{0} {1}  ({2})" -f $p.name, $p.version, $p.license))
        if ($p.repository) { [void]$sb.AppendLine($p.repository) }
        [void]$sb.AppendLine(("=" * 78))
        $dir = Split-Path $p.manifest_path
        $files = Get-ChildItem $dir -File | Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)' } | Sort-Object Name
        if (-not $files) {
            [void]$sb.AppendLine("(no license file in the package; license: $($p.license))")
        }
        foreach ($f in $files) {
            [void]$sb.AppendLine("--- $($f.Name) ---")
            [void]$sb.AppendLine((Get-Content $f.FullName -Raw))
        }
    }
    [IO.File]::WriteAllText((Join-Path $out "THIRD_PARTY_LICENSES.txt"), $sb.ToString(), (New-Object Text.UTF8Encoding $false))
    Write-Host ("[+] {0} crates" -f $deps.Count)

    # ---- zip ----
    if (-not $NoZip) {
        $zip = Join-Path $root "dist\$name.zip"
        if (Test-Path $zip) { Remove-Item $zip -Force }
        Compress-Archive -Path $out -DestinationPath $zip
        Write-Host "[+] $zip"
    }
    Write-Host "[+] $out"
    Get-ChildItem $out | Select-Object Name, Length | Format-Table -AutoSize
} finally { Pop-Location }
