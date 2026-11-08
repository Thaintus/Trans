function Get-TransVersionField {
    param([string]$Content, [string]$SectionPattern, [string]$Path)

    $sections = @([regex]::Matches($Content, $SectionPattern))
    $matching = @($sections | Where-Object { $_.Value -match '(?m)^[ \t]*name\s*=\s*"Trans"[ \t]*(?:#[^\r\n]*)?\r?$' })
    if ($matching.Count -ne 1) {
        throw "无法唯一定位 Trans 包信息：$Path"
    }
    $versions = @([regex]::Matches($matching[0].Value, '(?m)^[ \t]*version\s*=\s*"(?<version>[^"\r\n]+)"[^\r\n]*'))
    if ($versions.Count -ne 1) {
        throw "无法唯一定位 Trans 版本：$Path"
    }
    $version = $versions[0].Groups['version']
    return [pscustomobject]@{
        Version = $version.Value
        Offset = $matching[0].Index + $version.Index
        Length = $version.Length
    }
}

function Get-TransReleaseVersionPlan {
    param(
        [Parameter(Mandatory = $true)][string]$Workspace,
        [ValidateNotNullOrEmpty()][string]$Version
    )

    $encoding = [System.Text.UTF8Encoding]::new($false, $true)
    $definitions = @(
        @{ Name = 'Cargo.toml'; Pattern = '(?ms)^(?:\uFEFF)?\[package\][^\r\n]*\r?\n.*?(?=^\[|\z)' },
        @{ Name = 'Cargo.lock'; Pattern = '(?ms)^\[\[package\]\][^\r\n]*\r?\n.*?(?=^\[\[package\]\]|\z)' }
    )
    $files = foreach ($definition in $definitions) {
        $path = Join-Path $Workspace $definition.Name
        $bytes = [System.IO.File]::ReadAllBytes($path)
        $content = $encoding.GetString($bytes)
        $field = Get-TransVersionField -Content $content -SectionPattern $definition.Pattern -Path $path
        [pscustomobject]@{ Path = $path; OriginalBytes = $bytes; Content = $content; Field = $field }
    }
    $current = $files[0].Field.Version
    if ($files[1].Field.Version -cne $current) {
        throw 'Cargo.toml 与 Cargo.lock 的 Trans 版本不一致，请先修复版本信息'
    }
    $stableVersionPattern = '\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\z'
    if ($current -notmatch $stableVersionPattern) {
        throw "自动打包要求稳定的 major.minor.patch 版本：$current"
    }
    $currentNumber = $null
    if (-not [version]::TryParse($current, [ref]$currentNumber) -or
        $currentNumber.Major -gt 65535 -or $currentNumber.Minor -gt 65535 -or $currentNumber.Build -gt 65535) {
        throw "当前版本超出 Windows 文件版本字段范围：$current"
    }
    if ($PSBoundParameters.ContainsKey('Version')) {
        $requestedNumber = $null
        if ($Version -notmatch $stableVersionPattern -or
            -not [version]::TryParse($Version, [ref]$requestedNumber)) {
            throw "指定版本必须为稳定的 major.minor.patch 版本：$Version"
        }
        if ($requestedNumber.Major -gt 65535 -or $requestedNumber.Minor -gt 65535 -or $requestedNumber.Build -gt 65535) {
            throw "指定版本超出 Windows 文件版本字段范围：$Version"
        }
        if ($requestedNumber.CompareTo($currentNumber) -le 0) {
            throw "指定版本必须高于当前版本 ${current}：$Version"
        }
        $next = $Version
    } else {
        if ($currentNumber.Build -ge 65535) {
            throw '下一版本超出 Windows 文件版本字段范围，请手动调整主版本或次版本'
        }
        $next = '{0}.{1}.{2}' -f $currentNumber.Major, $currentNumber.Minor, ($currentNumber.Build + 1)
    }
    $changes = foreach ($file in $files) {
        $updated = $file.Content.Substring(0, $file.Field.Offset) + $next +
            $file.Content.Substring($file.Field.Offset + $file.Field.Length)
        [pscustomobject]@{
            Path = $file.Path
            OriginalBytes = $file.OriginalBytes
            UpdatedBytes = $encoding.GetBytes($updated)
        }
    }
    return [pscustomobject]@{ CurrentVersion = $current; Version = $next; Changes = @($changes) }
}

function Enter-TransReleaseLock {
    param([Parameter(Mandatory = $true)][string]$Workspace)

    $target = Join-Path $Workspace 'target'
    [System.IO.Directory]::CreateDirectory($target) | Out-Null
    $lockPath = Join-Path $target '.release-version.lock'
    try {
        return [System.IO.File]::Open($lockPath, [System.IO.FileMode]::OpenOrCreate,
            [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
    } catch [System.IO.IOException] {
        throw "无法取得打包锁，请确认同一项目没有其他打包任务：$lockPath"
    }
}

function Invoke-TransReleaseVersionTransaction {
    param(
        [Parameter(Mandatory = $true)][string]$Workspace,
        [Parameter(Mandatory = $true)][scriptblock]$Publish,
        [ValidateNotNullOrEmpty()][string]$Version
    )

    $gate = Enter-TransReleaseLock -Workspace $Workspace
    $plan = $null
    $committed = $false
    try {
        $planOptions = @{ Workspace = $Workspace }
        if ($PSBoundParameters.ContainsKey('Version')) {
            $planOptions.Version = $Version
        }
        $plan = Get-TransReleaseVersionPlan @planOptions
        foreach ($change in $plan.Changes) {
            [System.IO.File]::WriteAllBytes($change.Path, $change.UpdatedBytes)
        }
        & $Publish $plan.Version
        $committed = $true
    } finally {
        try {
            if (-not $committed -and $null -ne $plan) {
                $rollbackErrors = @()
                foreach ($change in $plan.Changes) {
                    try {
                        [System.IO.File]::WriteAllBytes($change.Path, $change.OriginalBytes)
                    } catch {
                        $rollbackErrors += $_.Exception.Message
                    }
                }
                if ($rollbackErrors.Count -ne 0) {
                    throw ('打包失败且版本恢复不完整：' + ($rollbackErrors -join '；'))
                }
            }
        } finally {
            $gate.Dispose()
        }
    }
}

function Get-TransReleaseDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$DistDirectory,
        [datetime]$Timestamp = (Get-Date)
    )

    $candidate = $Timestamp
    do {
        $path = Join-Path $DistDirectory ('release-' + $candidate.ToString('yyyyMMdd-HHmmss'))
        $candidate = $candidate.AddSeconds(1)
    } while (Test-Path -LiteralPath $path)
    return $path
}
