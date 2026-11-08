[CmdletBinding()]
param([string]$ArtifactDirectory)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-version.ps1')

if ([string]::IsNullOrWhiteSpace($ArtifactDirectory)) {
    $workspace = Split-Path -Parent $PSScriptRoot
    $ArtifactDirectory = Join-Path $workspace ('target\release-version-tests-' + [guid]::NewGuid().ToString('N'))
}
[System.IO.Directory]::CreateDirectory($ArtifactDirectory) | Out-Null
$script:checks = 0

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
    $script:checks += 1
}

function Assert-Fails {
    param([scriptblock]$Action, [string]$Expected)
    $caughtFailureMessage = $null
    try { & $Action } catch { $caughtFailureMessage = $_.Exception.Message }
    Assert-Condition ($null -ne $caughtFailureMessage -and $caughtFailureMessage.Contains($Expected)) "未捕获预期失败：$Expected，实际：$caughtFailureMessage"
}

function New-VersionFixture {
    param([string]$Name, [string]$Version = '2.0.0', [switch]$Bom)

    $fixture = Join-Path $ArtifactDirectory $Name
    [System.IO.Directory]::CreateDirectory($fixture) | Out-Null
    $encoding = [System.Text.UTF8Encoding]::new($Bom.IsPresent)
    $manifest = "[package]`r`nname = `"Trans`" # fixture`r`nversion = `"$Version`" # keep comment`r`nedition = `"2021`"`r`n`r`n[dependencies]`r`nexample = `"9.8.7`"`r`n"
    $lock = "version = 3`r`n`r`n[[package]]`r`nname = `"example`"`r`nversion = `"9.8.7`"`r`n`r`n[[package]]`r`nname = `"Trans`"`r`nversion = `"$Version`"`r`ndependencies = [`r`n `"example`",`r`n]`r`n"
    [System.IO.File]::WriteAllText((Join-Path $fixture 'Cargo.toml'), $manifest, $encoding)
    [System.IO.File]::WriteAllText((Join-Path $fixture 'Cargo.lock'), $lock, $encoding)
    return $fixture
}

function Get-FixtureBytes {
    param([string]$Fixture)
    return @('Cargo.toml', 'Cargo.lock') | ForEach-Object {
        [Convert]::ToBase64String([System.IO.File]::ReadAllBytes((Join-Path $Fixture $_)))
    }
}

$success = New-VersionFixture 'success' -Bom
Invoke-TransReleaseVersionTransaction -Workspace $success -Publish {
    param($version)
    Assert-Condition ($version -ceq '2.0.1') '首次包未递增到 2.0.1'
    $plan = Get-TransReleaseVersionPlan $success
    Assert-Condition ($plan.CurrentVersion -ceq '2.0.1') '构建回调读取的版本未同步'
}
$after = Get-TransReleaseVersionPlan $success
Assert-Condition ($after.CurrentVersion -ceq '2.0.1') '成功后未保留新版本'
$manifestBytes = [System.IO.File]::ReadAllBytes((Join-Path $success 'Cargo.toml'))
Assert-Condition ($manifestBytes[0] -eq 239 -and $manifestBytes[1] -eq 187 -and $manifestBytes[2] -eq 191) '丢失 UTF-8 BOM'
$manifestText = [System.IO.File]::ReadAllText((Join-Path $success 'Cargo.toml'))
Assert-Condition ($manifestText.Contains('version = "2.0.1" # keep comment')) '版本注释未保留'
Assert-Condition ($manifestText.Contains('example = "9.8.7"')) '错误修改依赖版本'
Assert-Condition ($manifestText.Contains("`r`n")) 'CRLF 换行未保留'
Invoke-TransReleaseVersionTransaction -Workspace $success -Publish {
    param($version)
    Assert-Condition ($version -ceq '2.0.2') '连续打包未继续递增'
}
Assert-Condition ((Get-TransReleaseVersionPlan $success).CurrentVersion -ceq '2.0.2') '第二次成功版本未保留'

$failure = New-VersionFixture 'publish-failure'
$original = @(Get-FixtureBytes $failure)
Assert-Fails {
    Invoke-TransReleaseVersionTransaction -Workspace $failure -Publish {
        param($version)
        Assert-Condition ($version -ceq '2.0.1') '失败回调未看到待发布版本'
        throw 'simulated publish failure'
    }
} 'simulated publish failure'
Assert-Condition ((@(Get-FixtureBytes $failure) -join ',') -ceq ($original -join ',')) '发布失败后版本文件未按原字节恢复'
Invoke-TransReleaseVersionTransaction -Workspace $failure -Publish {
    param($version)
    Assert-Condition ($version -ceq '2.0.1') '失败重试跳过版本或未释放锁'
}

$mismatch = New-VersionFixture 'mismatch'
$lockPath = Join-Path $mismatch 'Cargo.lock'
$text = [System.IO.File]::ReadAllText($lockPath).Replace('version = "2.0.0"', 'version = "2.0.5"')
[System.IO.File]::WriteAllText($lockPath, $text)
$original = @(Get-FixtureBytes $mismatch)
Assert-Fails { Invoke-TransReleaseVersionTransaction $mismatch { throw 'publish must not run' } } '版本不一致'
Assert-Condition ((@(Get-FixtureBytes $mismatch) -join ',') -ceq ($original -join ',')) '版本不一致时修改了文件'

foreach ($version in @('2.0.0-beta.1', '2.0.65535')) {
    $invalid = New-VersionFixture ('invalid-' + $version) $version
    $original = @(Get-FixtureBytes $invalid)
    Assert-Fails { Invoke-TransReleaseVersionTransaction $invalid { throw 'publish must not run' } } '版本'
    Assert-Condition ((@(Get-FixtureBytes $invalid) -join ',') -ceq ($original -join ',')) '无效版本被修改'
}

$concurrent = New-VersionFixture 'concurrent'
$original = @(Get-FixtureBytes $concurrent)
$gate = Enter-TransReleaseLock $concurrent
try {
    Assert-Fails { Invoke-TransReleaseVersionTransaction $concurrent { throw 'publish must not run' } } '打包锁'
    Assert-Condition ((@(Get-FixtureBytes $concurrent) -join ',') -ceq ($original -join ',')) '并发拒绝后版本被修改'
} finally {
    $gate.Dispose()
}

$dist = Join-Path $ArtifactDirectory 'collision-dist'
$timestamp = [datetime]::new(2026, 11, 5, 10, 20, 30)
$existing = Join-Path $dist 'release-20261105-102030'
[System.IO.Directory]::CreateDirectory($existing) | Out-Null
[System.IO.File]::WriteAllText((Join-Path $existing 'keep.txt'), 'old package')
$candidate = Get-TransReleaseDirectory -DistDirectory $dist -Timestamp $timestamp
Assert-Condition ((Split-Path -Leaf $candidate) -ceq 'release-20261105-102031') '发布路径碰撞未避让'
Assert-Condition ([System.IO.File]::ReadAllText((Join-Path $existing 'keep.txt')) -ceq 'old package') '覆盖了已有发布文件'

Write-Output "Release version tests passed ($script:checks checks)"
Write-Output "Artifacts: $ArtifactDirectory"
