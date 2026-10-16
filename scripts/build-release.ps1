[CmdletBinding()]
param()

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

$workspaceDir = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$manifestPath = Join-Path $workspaceDir 'Cargo.toml'
$lockPath = Join-Path $workspaceDir 'Cargo.lock'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "找不到 Cargo.toml：$manifestPath"
}
if (-not (Test-Path -LiteralPath $lockPath -PathType Leaf)) {
    throw '找不到 Cargo.lock，请先准备并审核依赖锁文件后再打包'
}

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

$targetTriple = 'x86_64-pc-windows-msvc'
$buildExe = Assert-WorkspacePath -Candidate (Join-Path $workspaceDir "target\$targetTriple\release\Trans.exe") -Workspace $workspaceDir
$distDir = Assert-WorkspacePath -Candidate (Join-Path $workspaceDir 'dist') -Workspace $workspaceDir
$distExe = Assert-WorkspacePath -Candidate (Join-Path $distDir 'Trans.exe') -Workspace $workspaceDir
$hashPath = Assert-WorkspacePath -Candidate (Join-Path $distDir 'Trans.exe.sha256') -Workspace $workspaceDir

$previousTargetDir = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = Join-Path $workspaceDir 'target'
    Push-Location -LiteralPath $workspaceDir
    try {
        Write-Host '开始构建 Windows x64 Release 单体 EXE'
        & $cargoPath build --release --locked --target $targetTriple
        if ($LASTEXITCODE -ne 0) {
            throw "Cargo 构建失败，退出码：$LASTEXITCODE"
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
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null
    Copy-Item -LiteralPath $buildExe -Destination $distExe -Force
    $sourceHash = (Get-FileHash -LiteralPath $buildExe -Algorithm SHA256).Hash
    $outputHash = (Get-FileHash -LiteralPath $distExe -Algorithm SHA256).Hash
    if ($sourceHash -ne $outputHash) {
        throw '复制后的 EXE 校验失败'
    }
    ($outputHash.ToLowerInvariant() + '  Trans.exe') | Set-Content -LiteralPath $hashPath -Encoding UTF8
    Write-Host "已生成：$distExe"
    Write-Host "SHA256：$outputHash"
} finally {
    $env:CARGO_TARGET_DIR = $previousTargetDir
}
