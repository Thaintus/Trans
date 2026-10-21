[CmdletBinding()]
param([switch]$AllowOnline)

$ErrorActionPreference = 'Stop'

function Assert-WorkspacePath {
    param(
        [Parameter(Mandatory = $true)][string]$Candidate,
        [Parameter(Mandatory = $true)][string]$Workspace
    )

    $fullCandidate = [System.IO.Path]::GetFullPath($Candidate)
    $fullWorkspace = [System.IO.Path]::GetFullPath($Workspace).TrimEnd('\', '/')
    $workspacePrefix = $fullWorkspace + [System.IO.Path]::DirectorySeparatorChar
    if (-not $fullCandidate.StartsWith($workspacePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "输出路径不在项目目录内：$fullCandidate"
    }
    return $fullCandidate
}

function Assert-X64GuiExecutable {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $reader = $null
    try {
        $reader = [System.IO.BinaryReader]::new($stream)
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
            throw "构建产物不是有效的 Windows EXE：$Path"
        }
        $stream.Seek(0x3C, [System.IO.SeekOrigin]::Begin) | Out-Null
        $peOffset = [long]$reader.ReadUInt32()
        if ($peOffset -lt 64 -or $peOffset + 24 -gt $stream.Length) {
            throw "构建产物的 PE 头位置无效：$Path"
        }
        $stream.Seek($peOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw "构建产物不是 x64 PE 文件：$Path"
        }
        $stream.Seek($peOffset + 20, [System.IO.SeekOrigin]::Begin) | Out-Null
        $optionalSize = $reader.ReadUInt16()
        $characteristics = $reader.ReadUInt16()
        $optionalOffset = $peOffset + 24
        if ($optionalSize -lt 112 -or $optionalOffset + $optionalSize -gt $stream.Length -or
            ($characteristics -band 0x0002) -eq 0 -or ($characteristics -band 0x2000) -ne 0) {
            throw "构建产物的可执行文件头无效：$Path"
        }
        $stream.Seek($optionalOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($reader.ReadUInt16() -ne 0x020B) {
            throw "构建产物不是 PE32+ 文件：$Path"
        }
        $stream.Seek($optionalOffset + 68, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($reader.ReadUInt16() -ne 2) {
            throw "构建产物不是 Windows GUI 程序：$Path"
        }
    } finally {
        if ($null -ne $reader) {
            $reader.Dispose()
        }
        $stream.Dispose()
    }
}

$workspaceDir = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$manifestPath = Join-Path $workspaceDir 'Cargo.toml'
$lockPath = Join-Path $workspaceDir 'Cargo.lock'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "找不到 Cargo.toml：$manifestPath"
}
if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
    throw '找不到 Cargo.lock，请先准备并审核依赖锁文件后再打包'
}

$targetTriple = 'x86_64-pc-windows-msvc'
$buildExe = Assert-WorkspacePath -Candidate (Join-Path $workspaceDir "target\$targetTriple\release\Trans.exe") -Workspace $workspaceDir
$distDir = Assert-WorkspacePath -Candidate (Join-Path $workspaceDir 'dist') -Workspace $workspaceDir

$previousTargetDir = $env:CARGO_TARGET_DIR
$previousOffline = $env:CARGO_NET_OFFLINE
$previousPath = $env:PATH
try {
    $cargoCommand = Get-Command 'cargo.exe' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $cargoCommand) {
        $cargoPath = $cargoCommand.Source
    } else {
        if ([string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
            throw '未找到 cargo.exe，且 USERPROFILE 未设置，请安装 Rust MSVC 工具链并加入 PATH'
        }
        $cargoBinDir = Join-Path $env:USERPROFILE '.cargo\bin'
        $cargoPath = Join-Path $cargoBinDir 'cargo.exe'
        if (-not (Test-Path -LiteralPath $cargoPath -PathType Leaf)) {
            throw '未找到 cargo.exe，请安装 Rust MSVC 工具链并加入 PATH'
        }
        $env:PATH = $cargoBinDir + [System.IO.Path]::PathSeparator + $env:PATH
    }
    $env:CARGO_TARGET_DIR = Join-Path $workspaceDir 'target'
    $env:CARGO_NET_OFFLINE = if ($AllowOnline) { 'false' } else { 'true' }
    Push-Location -LiteralPath $workspaceDir
    try {
        Write-Host '开始构建 Windows x64 Release 单体 EXE'
        & $cargoPath build --release --locked --target $targetTriple
        if ($LASTEXITCODE -ne 0) {
            $buildError = "Cargo 构建失败，退出码：$LASTEXITCODE"
            if (-not $AllowOnline) {
                $buildError += '；当前为离线模式，若本机缺少依赖缓存，可使用 -AllowOnline 允许联网后重试'
            }
            throw $buildError
        }
    } finally {
        Pop-Location
    }

    if (-not (Test-Path -LiteralPath $buildExe -PathType Leaf)) {
        throw "构建结束后未找到预期 EXE：$buildExe"
    }
    $buildItem = Get-Item -LiteralPath $buildExe
    if ($buildItem.Length -le 0) {
        throw "构建产物为空：$buildExe"
    }
    Assert-X64GuiExecutable -Path $buildExe
    $sourceHash = (Get-FileHash -LiteralPath $buildExe -Algorithm SHA256).Hash
    $releaseName = 'release-' + (Get-Date -Format 'yyyyMMdd-HHmmss')
    $releaseDir = Assert-WorkspacePath -Candidate (Join-Path $distDir $releaseName) -Workspace $workspaceDir
    $distExe = Assert-WorkspacePath -Candidate (Join-Path $releaseDir 'Trans.exe') -Workspace $workspaceDir
    $hashPath = Assert-WorkspacePath -Candidate (Join-Path $releaseDir 'Trans.exe.sha256') -Workspace $workspaceDir
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null
    New-Item -ItemType Directory -Path $releaseDir | Out-Null
    [System.IO.File]::Copy($buildExe, $distExe, $false)
    Assert-X64GuiExecutable -Path $distExe
    $outputHash = (Get-FileHash -LiteralPath $distExe -Algorithm SHA256).Hash
    if ($sourceHash -ne $outputHash) {
        throw '复制后的 EXE 校验失败'
    }
    ($outputHash.ToLowerInvariant() + '  Trans.exe') | Set-Content -LiteralPath $hashPath -Encoding UTF8
    Write-Host "已生成：$distExe"
    Write-Host "SHA256：$outputHash"
} finally {
    $env:CARGO_TARGET_DIR = $previousTargetDir
    $env:CARGO_NET_OFFLINE = $previousOffline
    $env:PATH = $previousPath
}
