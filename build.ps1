# build.ps1 — CurveEditor2 Rust プラグイン ビルド & デプロイ
#
# 使い方: .\build.ps1
#         .\build.ps1 -NoDeploy

param(
    [switch]$NoDeploy
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$SrcDir      = $PSScriptRoot
$AviUtl2Root = "C:\ProgramData\aviutl2"
$PluginDir   = "$AviUtl2Root\Plugin\CurveEditor2"
$ScriptDir   = "$AviUtl2Root\Script\CurveEditor2"
$AssetsDir   = Join-Path $SrcDir "assets"
$OldPluginDir = "$AviUtl2Root\Plugin\ScriptCurve"

Write-Host "`n=== CurveEditor2 ビルド (Rust) ===" -ForegroundColor Yellow

Push-Location $SrcDir
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed (exit $LASTEXITCODE)"
    }
} finally {
    Pop-Location
}

$ReleaseDir = Join-Path $SrcDir "target\release"
$DllName = "curve_editor2.dll"
$DllPath = Join-Path $ReleaseDir $DllName

if (-not (Test-Path $DllPath)) {
    Write-Error "ビルド成果物が見つかりません: $DllPath"
}

$Aux2Out = Join-Path $SrcDir "CurveEditor2.aux2"
Copy-Item -Path $DllPath -Destination $Aux2Out -Force
Write-Host "  完了: $Aux2Out" -ForegroundColor Green

if (-not $NoDeploy) {
    Write-Host "`n=== デプロイ ===" -ForegroundColor Yellow

    if (-not (Test-Path $PluginDir)) {
        New-Item -ItemType Directory -Path $PluginDir -Force | Out-Null
    }
    Copy-Item -Path $Aux2Out -Destination "$PluginDir\CurveEditor2.aux2" -Force
    Write-Host "  配置: $PluginDir\CurveEditor2.aux2" -ForegroundColor DarkGreen

    # トラックバー移動スクリプトの正本は assets/。全部の .tra2 を配置する
    $Tra2Files = @(Get-ChildItem -LiteralPath $AssetsDir -Filter "*.tra2" -File)
    if ($Tra2Files.Count -eq 0) {
        throw "assets に .tra2 がありません: $AssetsDir"
    }
    if (-not (Test-Path -LiteralPath $ScriptDir)) {
        New-Item -ItemType Directory -Path $ScriptDir -Force | Out-Null
    }
    foreach ($f in $Tra2Files) {
        Copy-Item -LiteralPath $f.FullName -Destination (Join-Path $ScriptDir $f.Name) -Force
        Write-Host "  配置: $ScriptDir\$($f.Name)" -ForegroundColor DarkGreen
    }

    # 旧 C++ エディタを退避（存在する場合）
    $OldEditor = Join-Path $OldPluginDir "ScriptCurveEditor.aux2"
    $ArchiveDir = "$AviUtl2Root\Plugin\oldScript\ScriptCurve"
    if (Test-Path $OldEditor) {
        if (-not (Test-Path $ArchiveDir)) {
            New-Item -ItemType Directory -Path $ArchiveDir -Force | Out-Null
        }
        $ArchivePath = Join-Path $ArchiveDir "ScriptCurveEditor.aux2"
        if (-not (Test-Path $ArchivePath)) {
            Move-Item -Path $OldEditor -Destination $ArchivePath -Force
            Write-Host "  退避: $ArchivePath" -ForegroundColor DarkYellow
        } else {
            Remove-Item -Path $OldEditor -Force
            Write-Host "  削除: 旧 ScriptCurveEditor.aux2 (退避済み)" -ForegroundColor DarkYellow
        }
    }
}

Write-Host "`n全ビルド完了`n" -ForegroundColor Yellow
