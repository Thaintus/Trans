param(
    [Parameter(Mandatory = $true)] [string] $ExePath,
    [Parameter(Mandatory = $true)] [string] $ArtifactDirectory
)

$ErrorActionPreference = 'Stop'
$exeFullPath = [IO.Path]::GetFullPath($ExePath)
$artifactFullPath = [IO.Path]::GetFullPath($ArtifactDirectory)
if (-not (Test-Path -LiteralPath $exeFullPath -PathType Leaf)) {
    throw "Executable does not exist: $exeFullPath"
}
if (@(Get-Process -Name Trans -ErrorAction SilentlyContinue).Count -ne 0) {
    throw 'Another Trans instance is running; this smoke runner does not close other instances'
}
$hostScriptPath = Join-Path $PSScriptRoot 'test-window-host.ps1'
if (-not (Test-Path -LiteralPath $hostScriptPath -PathType Leaf)) {
    throw "Synthetic window host script does not exist: $hostScriptPath"
}
New-Item -ItemType Directory -Path $artifactFullPath -Force | Out-Null
$profilePath = Join-Path $artifactFullPath 'profile'
$realProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
if ($profilePath.TrimEnd('\').Equals($realProfile.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The test profile must not use the real ApplicationData directory'
}
if (Test-Path -LiteralPath $profilePath) {
    throw "This smoke run requires a new isolated profile: $profilePath"
}
$settingsDirectory = Join-Path $profilePath 'Trans'
New-Item -ItemType Directory -Path $settingsDirectory -Force | Out-Null
$settingsPath = Join-Path $settingsDirectory 'settings.ini'
$fixture = "language=en`nclose_to_tray=true`nself_transparency=0`nshow_icons=false`nprivacy_mode=false`nrestore_hotkey=None`npreset_1=0`npreset_2=25`npreset_3=50`npreset_4=75`npreset_5=100`n"
[IO.File]::WriteAllText($settingsPath, $fixture, [Text.UTF8Encoding]::new($false))

if (-not ('TransWindowOpsNativeV1' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class TransWindowOpsNativeV1 {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr param);
    [StructLayout(LayoutKind.Sequential)] public struct RECT {
        public int Left, Top, Right, Bottom;
        public int Width { get { return Right - Left; } }
        public int Height { get { return Bottom - Top; } }
    }
    [StructLayout(LayoutKind.Sequential)] public struct COMBOBOXINFO {
        public uint Size;
        public RECT Item, Button;
        public uint ButtonState;
        public IntPtr Combo, Edit, List;
    }
    public sealed class LayeredInfo {
        public long ExtendedStyle;
        public bool Layered;
        public bool HasAttributes;
        public byte Alpha;
        public uint Flags;
        public bool Opaque { get { return !Layered || (HasAttributes && ((Flags & 2) == 0 || Alpha == 255)); } }
    }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr param);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc callback, IntPtr param);
    [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr GetWindowLongPtrW(IntPtr hwnd, int index);
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr SetWindowLongPtrW(IntPtr hwnd, int index, IntPtr value);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetLayeredWindowAttributes(IntPtr hwnd, uint key, byte alpha, uint flags);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("kernel32.dll")] static extern void SetLastError(uint error);
    [DllImport("user32.dll")] static extern bool GetLayeredWindowAttributes(IntPtr hwnd, out uint key, out byte alpha, out uint flags);
    [DllImport("user32.dll")] static extern bool GetComboBoxInfo(IntPtr hwnd, ref COMBOBOXINFO info);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(IntPtr hwnd, uint message, UIntPtr wp, IntPtr lp);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr SendRaw(IntPtr hwnd, uint message, UIntPtr wp, IntPtr lp, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr SetTextRaw(IntPtr hwnd, uint message, UIntPtr wp, string lp, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr GetTextRaw(IntPtr hwnd, uint message, UIntPtr wp, StringBuilder lp, uint flags, uint timeout, out UIntPtr result);
    public static long Send(IntPtr hwnd, uint message, ulong wp, long lp) {
        UIntPtr result;
        if (SendRaw(hwnd, message, new UIntPtr(wp), new IntPtr(lp), 2, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("SendMessageTimeout failed: message=" + message + " error=" + Marshal.GetLastWin32Error());
        return unchecked((long)result.ToUInt64());
    }
    public static void SetText(IntPtr hwnd, string value) {
        UIntPtr result;
        if (SetTextRaw(hwnd, 12, UIntPtr.Zero, value, 2, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("WM_SETTEXT failed: " + Marshal.GetLastWin32Error());
    }
    public static string Text(IntPtr hwnd) {
        var text = new StringBuilder(4096);
        UIntPtr result;
        if (GetTextRaw(hwnd, 13, new UIntPtr(4096), text, 2, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("WM_GETTEXT failed: " + Marshal.GetLastWin32Error());
        return text.ToString();
    }
    public static IntPtr Root(int process, string title) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr ignored) {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            if (owner == process && IsWindowVisible(hwnd) && Text(hwnd) == title) { found = hwnd; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static IntPtr Control(IntPtr parent, int id) {
        IntPtr found = IntPtr.Zero;
        EnumChildWindows(parent, delegate(IntPtr hwnd, IntPtr ignored) {
            if (GetDlgCtrlID(hwnd) == id) { found = hwnd; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static IntPtr ComboEdit(IntPtr combo) {
        var info = new COMBOBOXINFO(); info.Size = (uint)Marshal.SizeOf(typeof(COMBOBOXINFO));
        if (!GetComboBoxInfo(combo, ref info) || info.Edit == IntPtr.Zero)
            throw new InvalidOperationException("GetComboBoxInfo failed");
        return info.Edit;
    }
    public static LayeredInfo Layered(IntPtr hwnd) {
        if (!IsWindow(hwnd)) throw new InvalidOperationException("Synthetic target no longer exists");
        uint key, flags; byte alpha;
        var style = ExtendedStyle(hwnd);
        bool attributes = GetLayeredWindowAttributes(hwnd, out key, out alpha, out flags);
        return new LayeredInfo { ExtendedStyle = style, Layered = (style & 0x80000) != 0,
            HasAttributes = attributes, Alpha = alpha, Flags = flags };
    }
    public static RECT Bounds(IntPtr hwnd) {
        RECT rect; if (!GetWindowRect(hwnd, out rect)) throw new InvalidOperationException("GetWindowRect failed"); return rect;
    }
    static long ExtendedStyle(IntPtr hwnd) {
        SetLastError(0);
        var style = GetWindowLongPtrW(hwnd, -20).ToInt64();
        int error = Marshal.GetLastWin32Error();
        if (style == 0 && error != 0) throw new InvalidOperationException("GetWindowLongPtrW failed: " + error);
        return style;
    }
    public static void AssertOwnedTarget(IntPtr hwnd, int process) {
        if (!IsWindow(hwnd)) throw new InvalidOperationException("Synthetic target no longer exists");
        uint owner;
        if (GetWindowThreadProcessId(hwnd, out owner) == 0)
            throw new InvalidOperationException("GetWindowThreadProcessId failed: " + Marshal.GetLastWin32Error());
        if (owner != (uint)process)
            throw new InvalidOperationException("Refusing to mutate a window outside the launched synthetic host process");
    }
    static void SetOwnedStyle(IntPtr hwnd, int process, long style) {
        AssertOwnedTarget(hwnd, process);
        SetLastError(0);
        var previous = SetWindowLongPtrW(hwnd, -20, new IntPtr(style));
        int error = Marshal.GetLastWin32Error();
        if (previous == IntPtr.Zero && error != 0)
            throw new InvalidOperationException("SetWindowLongPtrW failed: " + error);
        if (!SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0, 0x0037))
            throw new InvalidOperationException("SetWindowPos failed: " + Marshal.GetLastWin32Error());
    }
    public static void SetTargetAlpha(IntPtr hwnd, int process, byte alpha) {
        AssertOwnedTarget(hwnd, process);
        long style = ExtendedStyle(hwnd);
        if ((style & 0x80000) == 0) SetOwnedStyle(hwnd, process, style | 0x80000);
        AssertOwnedTarget(hwnd, process);
        if (!SetLayeredWindowAttributes(hwnd, 0, alpha, 2))
            throw new InvalidOperationException("SetLayeredWindowAttributes failed: " + Marshal.GetLastWin32Error());
    }
    public static void RestoreTargetStyle(IntPtr hwnd, int process, long originalStyle) {
        SetOwnedStyle(hwnd, process, originalStyle);
        if (ExtendedStyle(hwnd) != originalStyle)
            throw new InvalidOperationException("The synthetic target did not restore its original extended style");
    }
    public static void ClearTargetLayered(IntPtr hwnd, int process) {
        AssertOwnedTarget(hwnd, process);
        SetOwnedStyle(hwnd, process, ExtendedStyle(hwnd) & ~0x80000L);
    }
    public static void ClearLayeredAndHideShow(IntPtr hwnd, int process) {
        ClearTargetLayered(hwnd, process);
        HideShowTarget(hwnd, process);
    }
    public static void HideShowTarget(IntPtr hwnd, int process) {
        AssertOwnedTarget(hwnd, process);
        if (!IsWindowVisible(hwnd)) throw new InvalidOperationException("Synthetic target must be visible before hide/show");
        // ShowWindow returns the previous visibility, so verify both the return and resulting visibility
        bool wasVisible = ShowWindow(hwnd, 0);
        if (!wasVisible || !IsWindow(hwnd) || IsWindowVisible(hwnd))
            throw new InvalidOperationException("The synthetic target did not hide successfully");
        AssertOwnedTarget(hwnd, process);
        bool wasHidden = !ShowWindow(hwnd, 4);
        if (!wasHidden || !IsWindow(hwnd) || !IsWindowVisible(hwnd))
            throw new InvalidOperationException("The same synthetic target did not show without activation");
        AssertOwnedTarget(hwnd, process);
    }
}
'@
}
$previousDpiContext = [TransWindowOpsNativeV1]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
if ($previousDpiContext -eq [IntPtr]::Zero) { throw 'Cannot set PER_MONITOR_AWARE_V2 for the test thread' }
Add-Type -AssemblyName System.Drawing
$checks = [Collections.Generic.List[object]]::new()
$screenshots = [Collections.Generic.List[string]]::new()
$transProcess = $null
$hostProcess = $null
$root = [IntPtr]::Zero
$target = [IntPtr]::Zero
$targetTitle = 'Trans-QA-' + [Guid]::NewGuid().ToString('N')
$failure = $null
$reportPath = Join-Path $artifactFullPath 'window-operations-results.json'

function Assert-Smoke([bool] $Condition, [string] $Name, $Details = $null) {
    $checks.Add([pscustomobject]@{ name = $Name; passed = $Condition; details = $Details })
    if (-not $Condition) { throw "Window operation assertion failed: $Name" }
}
function Wait-Smoke([scriptblock] $Predicate, [string] $Name, [int] $TimeoutMs = 7000) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        if (& $Predicate) { return }
        Start-Sleep -Milliseconds 50
    } while ($timer.ElapsedMilliseconds -lt $TimeoutMs)
    throw "Timed out: $Name"
}
function Observe-Smoke([scriptblock] $Predicate, [string] $Name, [int] $DurationMs = 2300) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        [TransWindowOpsNativeV1]::AssertOwnedTarget($target, $hostProcess.Id)
        if (-not (& $Predicate)) { throw "State changed while observing: $Name" }
        Start-Sleep -Milliseconds 100
    } while ($timer.ElapsedMilliseconds -lt $DurationMs)
    if (-not (& $Predicate)) { throw "State changed after observing: $Name" }
}
function Get-Control([int] $Id) {
    $control = [TransWindowOpsNativeV1]::Control($root, $Id)
    if ($control -eq [IntPtr]::Zero) { throw "Control $Id not found" }
    return $control
}
function Click-Control([int] $Id) {
    [void][TransWindowOpsNativeV1]::Send((Get-Control $Id), 0x00F5, 0, 0)
}
function Get-RowCount { return [TransWindowOpsNativeV1]::Send((Get-Control 128), 0x1004, 0, 0) }
function Select-TestWindow {
    $combo = Get-Control 101
    [TransWindowOpsNativeV1]::SetText([TransWindowOpsNativeV1]::ComboEdit($combo), $targetTitle)
    [void][TransWindowOpsNativeV1]::Send($root, 0x0111, (5 -shl 16 -bor 101), $combo.ToInt64())
    Wait-Smoke { [TransWindowOpsNativeV1]::Send($combo, 0x0146, 0, 0) -eq 1 } 'one synthetic dropdown candidate'
    [void][TransWindowOpsNativeV1]::Send($combo, 0x014F, 1, 0)
    [void][TransWindowOpsNativeV1]::Send($combo, 0x014E, 0, 0)
    [void][TransWindowOpsNativeV1]::Send($root, 0x0111, (9 -shl 16 -bor 101), $combo.ToInt64())
    [void][TransWindowOpsNativeV1]::Send($combo, 0x014F, 0, 0)
    Wait-Smoke { [TransWindowOpsNativeV1]::Text((Get-Control 103)) -eq $targetTitle -and (Get-RowCount) -eq 1 } 'synthetic window selected and tracked'
}
function Wait-Alpha([int] $Expected) {
    Wait-Smoke {
        $info = [TransWindowOpsNativeV1]::Layered($target)
        $info.Layered -and $info.HasAttributes -and ($info.Flags -band 2) -ne 0 -and [Math]::Abs([int]$info.Alpha - $Expected) -le 1
    } "synthetic window alpha $Expected"
}
function Save-Screenshot([string] $Name) {
    $path = Join-Path $artifactFullPath ($Name + '.png')
    $bounds = [TransWindowOpsNativeV1]::Bounds($root)
    $bitmap = [Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $hdc = $graphics.GetHdc()
        try {
            if (-not [TransWindowOpsNativeV1]::PrintWindow($root, $hdc, 0)) { throw 'PrintWindow failed' }
        } finally { $graphics.ReleaseHdc($hdc) }
        $bitmap.Save($path, [Drawing.Imaging.ImageFormat]::Png)
        $screenshots.Add($path)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
}

try {
    $hostInfo = [Diagnostics.ProcessStartInfo]::new()
    $hostInfo.FileName = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $hostInfo.Arguments = '-NoProfile -ExecutionPolicy Bypass -STA -File "' + $hostScriptPath + '" -Title "' + $targetTitle + '"'
    $hostInfo.UseShellExecute = $false
    $hostInfo.CreateNoWindow = $true
    $hostInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $hostProcess = [Diagnostics.Process]::Start($hostInfo)
    Wait-Smoke { [TransWindowOpsNativeV1]::Root($hostProcess.Id, $targetTitle) -ne [IntPtr]::Zero } 'synthetic host ready'
    $target = [TransWindowOpsNativeV1]::Root($hostProcess.Id, $targetTitle)
    [TransWindowOpsNativeV1]::AssertOwnedTarget($target, $hostProcess.Id)
    $initialSnapshot = [TransWindowOpsNativeV1]::Layered($target)
    Assert-Smoke ($initialSnapshot.Opaque -and -not $initialSnapshot.Layered) 'synthetic window starts opaque'

    $transInfo = [Diagnostics.ProcessStartInfo]::new()
    $transInfo.FileName = $exeFullPath
    $transInfo.WorkingDirectory = [IO.Path]::GetDirectoryName($exeFullPath)
    $transInfo.UseShellExecute = $false
    $transInfo.EnvironmentVariables['APPDATA'] = $profilePath
    $transProcess = [Diagnostics.Process]::Start($transInfo)
    Wait-Smoke { [TransWindowOpsNativeV1]::Root($transProcess.Id, 'Trans') -ne [IntPtr]::Zero } 'Trans ready'
    $root = [TransWindowOpsNativeV1]::Root($transProcess.Id, 'Trans')
    Assert-Smoke ($transProcess.Id -ne $hostProcess.Id) 'target is a separate synthetic process'
    Select-TestWindow
    [TransWindowOpsNativeV1]::SetTargetAlpha($target, $hostProcess.Id, 64)
    Observe-Smoke {
        $info = [TransWindowOpsNativeV1]::Layered($target)
        (Get-RowCount) -eq 1 -and $info.Layered -and $info.HasAttributes -and ($info.Flags -band 2) -ne 0 -and $info.Alpha -eq 64
    } 'selection alone leaves external alpha unchanged while idle'
    [TransWindowOpsNativeV1]::RestoreTargetStyle($target, $hostProcess.Id, $initialSnapshot.ExtendedStyle)
    Assert-Smoke ((Get-RowCount) -eq 1 -and [TransWindowOpsNativeV1]::Layered($target).Opaque) 'selection adds a row before transparency changes' ([pscustomobject]@{
        selectionOnlyAlpha = 64; observedMs = 2300; selectionDidNotEnforceTransparency = $true
    })

    Click-Control 112
    Wait-Alpha 128
    [TransWindowOpsNativeV1]::ClearLayeredAndHideShow($target, $hostProcess.Id)
    Wait-Alpha 128
    Assert-Smoke ($true) '50 percent preset changes real target alpha' ([pscustomobject]@{
        snapshot = [TransWindowOpsNativeV1]::Layered($target); recoveredAfterLayerClearAndHideShow = $true
    })
    Save-Screenshot 'window-50-percent'
    Click-Control 120
    Wait-Smoke { [TransWindowOpsNativeV1]::Layered($target).Opaque -and (Get-RowCount) -eq 0 } 'restore current removes row and restores opacity'
    Observe-Smoke {
        $info = [TransWindowOpsNativeV1]::Layered($target)
        $info.Opaque -and $info.ExtendedStyle -eq $initialSnapshot.ExtendedStyle -and (Get-RowCount) -eq 0
    } 'restore current leaves original opacity unchanged after the event-check window ends'
    Assert-Smoke ($true) 'restore current restores opacity and removes row' ([pscustomobject]@{
        snapshot = [TransWindowOpsNativeV1]::Layered($target); observedMs = 2300; maintenanceStopped = $true
    })

    Select-TestWindow
    Click-Control 111
    Wait-Alpha 191
    [TransWindowOpsNativeV1]::HideShowTarget($target, $hostProcess.Id)
    Start-Sleep -Milliseconds 80
    [TransWindowOpsNativeV1]::SetTargetAlpha($target, $hostProcess.Id, 255)
    Wait-Alpha 191
    Start-Sleep -Milliseconds 1200
    [TransWindowOpsNativeV1]::SetTargetAlpha($target, $hostProcess.Id, 200)
    Observe-Smoke {
        $info = [TransWindowOpsNativeV1]::Layered($target)
        (Get-RowCount) -eq 1 -and $info.Layered -and $info.HasAttributes -and ($info.Flags -band 2) -ne 0 -and $info.Alpha -eq 200
    } 'display refresh leaves stationary external alpha unchanged after the event-check window ends'
    [TransWindowOpsNativeV1]::ClearTargetLayered($target, $hostProcess.Id)
    Observe-Smoke {
        $info = [TransWindowOpsNativeV1]::Layered($target)
        (Get-RowCount) -eq 1 -and -not $info.Layered -and $info.Opaque
    } 'display refresh leaves a stationary cleared layered style unchanged until a new event'
    [TransWindowOpsNativeV1]::HideShowTarget($target, $hostProcess.Id)
    Wait-Alpha 191
    Assert-Smoke ($true) '25 percent preset restores alpha only during event-triggered checks' ([pscustomobject]@{
        snapshot = [TransWindowOpsNativeV1]::Layered($target)
        recoveredAfterExternalAlphaReset = $true
        externalResetDelayMs = 80
        eventRoundCompletedWaitMs = 1200
        idleExternalAlpha = 200
        idleObservedMs = 2300
        idleAlphaPreservedWithoutEvent = $true
        idleLayeredClearObservedMs = 2300
        idleLayeredClearPreservedWithoutEvent = $true
        recoveredAfterNextShowEvent = $true
    })
    Click-Control 121
    Wait-Smoke { [TransWindowOpsNativeV1]::Layered($target).Opaque -and (Get-RowCount) -eq 0 } 'restore all removes row and restores opacity'
    Observe-Smoke {
        $info = [TransWindowOpsNativeV1]::Layered($target)
        $info.Opaque -and $info.ExtendedStyle -eq $initialSnapshot.ExtendedStyle -and (Get-RowCount) -eq 0
    } 'restore all leaves original opacity unchanged after the event-check window ends'
    Assert-Smoke ($true) 'restore all restores opacity and removes row' ([pscustomobject]@{
        snapshot = [TransWindowOpsNativeV1]::Layered($target); observedMs = 2300; maintenanceStopped = $true
    })

    Select-TestWindow
    Click-Control 112
    Wait-Alpha 128
    [void][TransWindowOpsNativeV1]::PostMessage($root, 0x0111, [UIntPtr]::new(146), [IntPtr]::Zero)
    Wait-Smoke { $transProcess.HasExited } 'normal Trans exit' 7000
    Wait-Smoke { [TransWindowOpsNativeV1]::Layered($target).Opaque } 'normal exit restores target opacity'
    Assert-Smoke ($true) 'normal exit restores the modified synthetic target' ([TransWindowOpsNativeV1]::Layered($target))
} catch {
    $failure = $_.Exception.ToString()
} finally {
    if ($null -ne $transProcess -and -not $transProcess.HasExited) {
        if ($root -eq [IntPtr]::Zero) { $root = [TransWindowOpsNativeV1]::Root($transProcess.Id, 'Trans') }
        if ($root -ne [IntPtr]::Zero) {
            [void][TransWindowOpsNativeV1]::PostMessage($root, 0x0111, [UIntPtr]::new(146), [IntPtr]::Zero)
        }
        if (-not $transProcess.WaitForExit(7000)) {
            $transProcess.Kill()
            [void]$transProcess.WaitForExit(3000)
            if ($null -eq $failure) { $failure = 'Test Trans process did not exit normally during cleanup' }
        }
    }
    if ($null -ne $hostProcess -and -not $hostProcess.HasExited) {
        if ($target -ne [IntPtr]::Zero -and [TransWindowOpsNativeV1]::IsWindow($target)) {
            [void][TransWindowOpsNativeV1]::PostMessage($target, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero)
        }
        if (-not $hostProcess.WaitForExit(3000)) {
            $hostProcess.Kill()
            [void]$hostProcess.WaitForExit(3000)
        }
    }
    [void][TransWindowOpsNativeV1]::SetThreadDpiAwarenessContext($previousDpiContext)
    $report = [pscustomobject]@{
        passed = ($null -eq $failure)
        exe = $exeFullPath
        sha256 = (Get-FileHash -LiteralPath $exeFullPath -Algorithm SHA256).Hash
        profile = $profilePath
        targetTitle = $targetTitle
        targetProcess = $(if ($null -ne $hostProcess) { $hostProcess.Id } else { $null })
        transProcess = $(if ($null -ne $transProcess) { $transProcess.Id } else { $null })
        checks = @($checks.ToArray())
        screenshots = @($screenshots.ToArray())
        failure = $failure
        finishedAt = [DateTimeOffset]::Now.ToString('o')
    }
    [IO.File]::WriteAllText($reportPath, ($report | ConvertTo-Json -Depth 7), [Text.UTF8Encoding]::new($false))
}
$report | ConvertTo-Json -Depth 7
if ($null -ne $failure) { throw "Window operations smoke failed; details: $reportPath" }
