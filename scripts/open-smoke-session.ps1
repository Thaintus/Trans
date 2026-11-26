if (-not ('TransSmokeWindowV1' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class TransSmokeWindowV1 {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    static extern IntPtr FindWindow(string windowClass, string title);
    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")]
    static extern bool IsWindowVisible(IntPtr hwnd);

    public static IntPtr Find(int expectedProcess, bool requireVisible) {
        IntPtr hwnd = FindWindow("Trans.Smoke.MainWindow.1", null);
        uint process;
        if (hwnd == IntPtr.Zero || GetWindowThreadProcessId(hwnd, out process) == 0 ||
            process != expectedProcess || (requireVisible && !IsWindowVisible(hwnd))) return IntPtr.Zero;
        return hwnd;
    }
}
'@
}

function Open-SmokeSession([string] $ExePath) {
    $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($ExePath).FileVersion
    if (-not $version -or [version]$version -lt [version]'3.2.1') {
        throw "The smoke runner requires Trans 3.2.1 or later with isolated test windows. Found '$version'."
    }

    $created = $false
    $session = [Threading.Mutex]::new($false, 'Local\Trans.Smoke.Runner.1', [ref]$created)
    if (-not $created) {
        $session.Dispose()
        throw 'Another Trans smoke runner is already using this desktop'
    }
    try {
        $existing = $null
        if ([Threading.Mutex]::TryOpenExisting('Local\Trans.Smoke.MainWindow.1', [ref]$existing)) {
            $existing.Dispose()
            throw 'A Trans smoke window is still running from an earlier session'
        }
    } catch {
        $session.Dispose()
        throw
    }
    return $session
}

function Invoke-SmokeCleanup([scriptblock] $Action) {
    try {
        & $Action
    } catch {
        $message = $_.Exception.ToString()
        $script:cleanupErrors.Add($message)
        if ($null -eq $script:failure) { $script:failure = $message }
    }
}

function Stop-SmokeProcess([Diagnostics.Process] $Child, [scriptblock] $RequestClose,
                           [int] $WaitMs, [switch] $RequireNormalExit) {
    if ($null -eq $Child -or $Child.HasExited) { return }
    $closeError = $null
    try {
        & $RequestClose
        if ($Child.WaitForExit($WaitMs)) { return }
    } catch {
        $closeError = $_
    }
    $forced = $false
    try {
        if (-not $Child.HasExited) {
            try {
                $Child.Kill()
                $forced = $true
            } catch { if (-not $Child.HasExited) { throw } }
        }
        if (-not $Child.WaitForExit(3000)) {
            throw "The isolated process $($Child.Id) did not terminate"
        }
    } catch {
        $terminationError = "Could not terminate isolated PID $($Child.Id): $($_.Exception.Message)"
        if ($null -eq $closeError) { throw $terminationError }
        $script:cleanupErrors.Add($terminationError)
    }
    if ($null -ne $closeError) {
        throw "Could not close isolated PID $($Child.Id): $($closeError.Exception.Message)"
    }
    if ($RequireNormalExit -and $forced) { throw "The isolated process $($Child.Id) required forced cleanup" }
}
