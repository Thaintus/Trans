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
$profilePath = Join-Path $artifactFullPath 'profile'
$realProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
if ($profilePath.TrimEnd('\').Equals($realProfile.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The test profile must not use the real ApplicationData directory'
}
New-Item -ItemType Directory -Path $artifactFullPath -Force | Out-Null
$settingsDirectory = Join-Path $profilePath 'Trans'
$settingsPath = Join-Path $settingsDirectory 'settings.ini'
$markerPath = Join-Path $profilePath '.trans-smoke-profile'
if ((Test-Path -LiteralPath $profilePath) -and -not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
    throw "Refusing to reuse an unmarked profile: $profilePath"
}
New-Item -ItemType Directory -Path $settingsDirectory -Force | Out-Null
[IO.File]::WriteAllText($markerPath, 'Dedicated Trans Win32 smoke profile', [Text.UTF8Encoding]::new($false))
$fixture = "; Legacy Trans settings`nclose_to_tray=true`nself_transparency=0`nshow_icons=false`nprivacy_mode=false`nrestore_hotkey=None`npreset_1=0`npreset_2=25`npreset_3=50`npreset_4=75`npreset_5=100`n"
[IO.File]::WriteAllText($settingsPath, $fixture, [Text.UTF8Encoding]::new($false))

if (-not ('TransSmokeNativeV1' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class TransSmokeNativeV1 {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr param);
    [StructLayout(LayoutKind.Sequential)] public struct RECT {
        public int Left, Top, Right, Bottom;
        public int Width { get { return Right - Left; } }
        public int Height { get { return Bottom - Top; } }
    }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct SIZE { public int Width, Height; }
    [StructLayout(LayoutKind.Sequential, CharSet=System.Runtime.InteropServices.CharSet.Unicode)] public struct LOGFONT {
        public int Height, Width, Escapement, Orientation, Weight;
        public byte Italic, Underline, StrikeOut, CharSet, OutPrecision, ClipPrecision, Quality, PitchAndFamily;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string FaceName;
    }
    public struct TextMeasurement { public int Width, Height, FontHeight; public string FontFace; }
    public struct TextBufferResult { public long Copied; public string Text; public ushort[] Units; public bool TerminatorPresent, CanaryUnchanged; }
    [StructLayout(LayoutKind.Sequential)] public struct MINMAXINFO {
        public POINT Reserved, MaxSize, MaxPosition, MinTrackSize, MaxTrackSize;
    }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr param);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc callback, IntPtr param);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hwnd, uint command);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint key, uint type);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr hwnd);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr hwnd, IntPtr dc);
    [DllImport("gdi32.dll", EntryPoint="GetObjectW", CharSet=CharSet.Unicode)] static extern int GetFontObject(IntPtr font, int size, out LOGFONT value);
    [DllImport("gdi32.dll", CharSet=CharSet.Unicode)] static extern IntPtr CreateFontIndirect(ref LOGFONT value);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr value);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr value);
    [DllImport("gdi32.dll", CharSet=CharSet.Unicode)] static extern bool GetTextExtentPoint32(IntPtr dc, string text, int count, out SIZE size);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(IntPtr hwnd, uint message, UIntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr SendRaw(IntPtr hwnd, uint message, UIntPtr wp, IntPtr lp, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr SendTextRaw(IntPtr hwnd, uint message, UIntPtr wp, string lp, uint flags, uint timeout, out UIntPtr result);
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
        if (SendTextRaw(hwnd, 0x000C, UIntPtr.Zero, value, 2, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("WM_SETTEXT failed: " + Marshal.GetLastWin32Error());
    }
    public static string Text(IntPtr hwnd) {
        var text = new StringBuilder(4096);
        UIntPtr result;
        if (GetTextRaw(hwnd, 0x000D, new UIntPtr(4096), text, 2, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("WM_GETTEXT failed: " + Marshal.GetLastWin32Error());
        return text.ToString();
    }
    public static string ComboItemText(IntPtr hwnd, int index) {
        long length = Send(hwnd, 0x0149, (ulong)index, 0);
        if (length < 0) throw new InvalidOperationException("CB_GETLBTEXTLEN failed");
        var text = new StringBuilder((int)length + 1);
        UIntPtr result;
        if (GetTextRaw(hwnd, 0x0148, new UIntPtr((uint)index), text, 2, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("CB_GETLBTEXT failed");
        return text.ToString();
    }
    public static void Key(IntPtr hwnd, uint key) {
        long keyDown = 1L | ((long)MapVirtualKey(key, 0) << 16);
        Send(hwnd, 0x0100, key, keyDown);
        Send(hwnd, 0x0101, key, keyDown | 0xC0000000L);
    }
    public static TextBufferResult ReadTextBuffer(IntPtr hwnd, int capacity) {
        IntPtr memory = Marshal.AllocHGlobal((capacity + 1) * 2);
        try {
            for (int i=0; i<=capacity; i++) Marshal.WriteInt16(memory, i*2, 0x5A5A);
            long copied = Send(hwnd, 0x000D, (ulong)capacity, memory.ToInt64());
            var units = new ushort[capacity + 1];
            for (int i=0; i<=capacity; i++) units[i] = unchecked((ushort)Marshal.ReadInt16(memory, i*2));
            var text = new StringBuilder();
            int count = 0;
            while (count < capacity && units[count] != 0) { text.Append((char)units[count]); count++; }
            return new TextBufferResult { Copied = copied, Text = text.ToString(), Units = units,
                TerminatorPresent = count < capacity && units[count] == 0, CanaryUnchanged = units[capacity] == 0x5A5A };
        } finally { Marshal.FreeHGlobal(memory); }
    }
    public static string Class(IntPtr hwnd) {
        var text = new StringBuilder(256); GetClassName(hwnd, text, text.Capacity); return text.ToString();
    }
    public static IntPtr FindRoot(int process) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr ignored) {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            if (owner == process && IsWindowVisible(hwnd) && Class(hwnd) != "#32770" && Text(hwnd) == "Trans") {
                found = hwnd; return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static IntPtr FindDialog(int process) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr hwnd, IntPtr ignored) {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            if (owner == process && IsWindowVisible(hwnd) && Class(hwnd) == "#32770") { found = hwnd; return false; }
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
    public static IntPtr FindClass(IntPtr parent, string name) {
        IntPtr found = IntPtr.Zero;
        EnumChildWindows(parent, delegate(IntPtr hwnd, IntPtr ignored) {
            if (Class(hwnd) == name) { found = hwnd; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static string[] ChildTexts(IntPtr parent) {
        var texts = new List<string>();
        EnumChildWindows(parent, delegate(IntPtr hwnd, IntPtr ignored) { texts.Add(Text(hwnd)); return true; }, IntPtr.Zero);
        return texts.ToArray();
    }
    public static string Describe(IntPtr hwnd) {
        if (hwnd == IntPtr.Zero) return "No dialog";
        uint process; uint thread = GetWindowThreadProcessId(hwnd, out process);
        IntPtr owner = GetWindow(hwnd, 4);
        uint ownerProcess = 0;
        if (owner != IntPtr.Zero) GetWindowThreadProcessId(owner, out ownerProcess);
        return "hwnd=0x" + hwnd.ToInt64().ToString("X") + "; process=" + process + "; thread=" + thread
            + "; owner=0x" + owner.ToInt64().ToString("X") + "; ownerProcess=" + ownerProcess
            + "; valid=" + IsWindow(hwnd) + "; visible=" + IsWindowVisible(hwnd)
            + "; class=" + Class(hwnd) + "; caption=" + Text(hwnd)
            + "; children=" + String.Join(" | ", ChildTexts(hwnd));
    }
    public static RECT Bounds(IntPtr hwnd) {
        RECT rect; if (!GetWindowRect(hwnd, out rect)) throw new InvalidOperationException("GetWindowRect failed"); return rect;
    }
    public static RECT ClientBounds(IntPtr hwnd) {
        RECT rect; if (!GetClientRect(hwnd, out rect)) throw new InvalidOperationException("GetClientRect failed"); return rect;
    }
    public static MINMAXINFO Minimum(IntPtr hwnd) {
        int size = Marshal.SizeOf(typeof(MINMAXINFO));
        IntPtr memory = Marshal.AllocHGlobal(size);
        try {
            Marshal.StructureToPtr(new MINMAXINFO(), memory, false);
            Send(hwnd, 0x0024, 0, memory.ToInt64());
            return (MINMAXINFO)Marshal.PtrToStructure(memory, typeof(MINMAXINFO));
        } finally { Marshal.FreeHGlobal(memory); }
    }
    public static TextMeasurement MeasureControlText(IntPtr hwnd, string text) {
        IntPtr sourceFont = new IntPtr(Send(hwnd, 0x0031, 0, 0));
        LOGFONT data;
        if (sourceFont == IntPtr.Zero || GetFontObject(sourceFont, Marshal.SizeOf(typeof(LOGFONT)), out data) == 0)
            throw new InvalidOperationException("Could not read the control font returned by WM_GETFONT");
        IntPtr font = CreateFontIndirect(ref data);
        if (font == IntPtr.Zero) throw new InvalidOperationException("Could not clone the control font");
        IntPtr dc = GetDC(hwnd);
        if (dc == IntPtr.Zero) { DeleteObject(font); throw new InvalidOperationException("GetDC failed for font measurement"); }
        IntPtr previous = SelectObject(dc, font);
        try {
            SIZE size;
            if (previous == IntPtr.Zero || previous == new IntPtr(-1) || !GetTextExtentPoint32(dc, text, text.Length, out size))
                throw new InvalidOperationException("GetTextExtentPoint32 failed for the native control font");
            return new TextMeasurement { Width = size.Width, Height = size.Height, FontHeight = data.Height, FontFace = data.FaceName };
        } finally {
            if (previous != IntPtr.Zero && previous != new IntPtr(-1)) SelectObject(dc, previous);
            ReleaseDC(hwnd, dc); DeleteObject(font);
        }
    }
}
'@
}
[void][TransSmokeNativeV1]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))
$previousDpiContext = [TransSmokeNativeV1]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
if ($previousDpiContext -eq [IntPtr]::Zero) {
    throw 'Could not set PER_MONITOR_AWARE_V2 on the smoke runner thread'
}
Add-Type -AssemblyName System.Drawing

$checks = [Collections.Generic.List[object]]::new()
$shots = [Collections.Generic.List[string]]::new()
$dialogDiagnostics = [Collections.Generic.List[object]]::new()
$testProcess = $null
$root = [IntPtr]::Zero
$failure = $null
$backupPath = Join-Path $settingsDirectory 'settings.ini.smoke-backup'
$faultDirectoryCreated = $false

function Assert-Smoke([bool] $Condition, [string] $Name, $Details = $null) {
    $checks.Add([pscustomobject]@{ name = $Name; passed = $Condition; details = $Details })
    if (-not $Condition) { throw "GUI assertion failed: $Name" }
}

function Wait-Smoke([scriptblock] $Predicate, [string] $Name, [int] $TimeoutMs = 7000) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        if (& $Predicate) { return }
        Start-Sleep -Milliseconds 50
    } while ($timer.ElapsedMilliseconds -lt $TimeoutMs)
    throw "Timed out: $Name"
}

function Record-Dialog([string] $Stage, [IntPtr] $OriginalDialog) {
    $currentDialog = [TransSmokeNativeV1]::FindDialog($testProcess.Id)
    $snapshot = [pscustomobject]@{
        stage = $Stage
        expectedProcess = $testProcess.Id
        originalHandle = ('0x{0:X}' -f $OriginalDialog.ToInt64())
        originalStillValid = [TransSmokeNativeV1]::IsWindow($OriginalDialog)
        originalStillVisible = [TransSmokeNativeV1]::IsWindowVisible($OriginalDialog)
        currentHandle = ('0x{0:X}' -f $currentDialog.ToInt64())
        current = [TransSmokeNativeV1]::Describe($currentDialog)
    }
    $dialogDiagnostics.Add($snapshot)
    $path = Join-Path $artifactFullPath 'dialog-diagnostics.json'
    [IO.File]::WriteAllText($path, ($dialogDiagnostics.ToArray() | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
}

function Get-Control([int] $Id) {
    $control = [TransSmokeNativeV1]::Control($root, $Id)
    if ($control -eq [IntPtr]::Zero) { throw "Control $Id not found" }
    return $control
}

function Get-Caption([int] $Id) { return [TransSmokeNativeV1]::Text((Get-Control $Id)) }

function Click-Control([int] $Id) {
    [void][TransSmokeNativeV1]::Send((Get-Control $Id), 0x00F5, 0, 0)
    Start-Sleep -Milliseconds 120
}

function Select-Language([int] $Index, [switch] $Async) {
    if ($Index -notin @(0, 1)) { throw "Unknown language index: $Index" }
    $combo = Get-Control 153
    $selected = [TransSmokeNativeV1]::Send($combo, 0x014E, [uint64]$Index, 0)
    if ($selected -ne $Index) { throw 'CB_SETCURSEL failed for the language dropdown' }
    $parent = [TransSmokeNativeV1]::GetParent($combo)
    $notification = ([uint32]9 -shl 16) -bor [uint32]153
    if ($Async) {
        if (-not [TransSmokeNativeV1]::PostMessage($parent, 0x0111, [UIntPtr]::new($notification), $combo)) {
            throw 'Could not post the language selection notification'
        }
    } else {
        [void][TransSmokeNativeV1]::Send($parent, 0x0111, $notification, $combo.ToInt64())
        $caption = if ($Index -eq 0) { '设置' } else { 'Settings' }
        Wait-Smoke { (Get-Caption 140) -eq $caption } 'language selection applied'
    }
}

function Assert-EmptyHotkey([string] $Expected, [bool] $Enabled, [string] $Name, [switch] $CheckBuffer) {
    $hotkey = Get-Control 148
    $text = [TransSmokeNativeV1]::Text($hotkey)
    $length = [TransSmokeNativeV1]::Send($hotkey, 0x000E, 0, 0)
    $value = [TransSmokeNativeV1]::Send($hotkey, 0x0402, 0, 0)
    $controlEnabled = [TransSmokeNativeV1]::IsWindowEnabled($hotkey)
    Assert-Smoke ($text -eq $Expected -and $length -eq $Expected.Length -and $value -eq 0 -and $controlEnabled -eq $Enabled) $Name @{ text = $text; expectedText = $Expected; length = $length; hotkeyValue = $value; enabled = $controlEnabled }
    if (-not $CheckBuffer) { return }
    $zero = [TransSmokeNativeV1]::ReadTextBuffer($hotkey, 0)
    $one = [TransSmokeNativeV1]::ReadTextBuffer($hotkey, 1)
    $short = [TransSmokeNativeV1]::ReadTextBuffer($hotkey, 2)
    $expectedFirst = $Expected.Substring(0, 1)
    Assert-Smoke ($zero.Copied -eq 0 -and $zero.CanaryUnchanged -and $one.Copied -eq 0 -and $one.TerminatorPresent -and $one.CanaryUnchanged -and $short.Copied -eq 1 -and $short.Text -eq $expectedFirst -and $short.TerminatorPresent -and $short.CanaryUnchanged) ($Name + '_bounded_text') @{ zero = $zero; one = $one; short = $short }
}

function Save-Screenshot([string] $Name) {
    $path = Join-Path $artifactFullPath ($Name + '.png')
    $bounds = [TransSmokeNativeV1]::Bounds($root)
    $bitmap = [Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    try {
        if (-not [TransSmokeNativeV1]::PrintWindow($root, $hdc, 0)) { throw 'PrintWindow failed' }
    } finally { $graphics.ReleaseHdc($hdc) }
    try { $bitmap.Save($path, [Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
    $shots.Add($path)
}

function Start-TestApplication {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $exeFullPath
    $start.WorkingDirectory = [IO.Path]::GetDirectoryName($exeFullPath)
    $start.UseShellExecute = $false
    $start.EnvironmentVariables['APPDATA'] = $profilePath
    $script:testProcess = [Diagnostics.Process]::Start($start)
    Wait-Smoke { $script:root = [TransSmokeNativeV1]::FindRoot($testProcess.Id); $root -ne [IntPtr]::Zero } 'Trans main window'
    Assert-Smoke (-not $testProcess.HasExited) 'isolated_application_started' @{ processId = $testProcess.Id; profile = $profilePath }
}

function Stop-TestApplication {
    if ($null -eq $testProcess -or $testProcess.HasExited) { return }
    $dialog = [TransSmokeNativeV1]::FindDialog($testProcess.Id)
    if ($dialog -ne [IntPtr]::Zero) {
        [void][TransSmokeNativeV1]::PostMessage($dialog, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 150
    }
    if ($root -ne [IntPtr]::Zero -and [TransSmokeNativeV1]::IsWindow($root)) {
        $exit = [TransSmokeNativeV1]::Control($root, 146)
        if ($exit -ne [IntPtr]::Zero) {
            [void][TransSmokeNativeV1]::PostMessage($exit, 0x00F5, [UIntPtr]::Zero, [IntPtr]::Zero)
        }
    }
    if (-not $testProcess.WaitForExit(5000)) {
        $testProcess.Kill()
        $testProcess.WaitForExit()
        throw 'The isolated test process required forced cleanup'
    }
}

try {
    Start-TestApplication
    Assert-Smoke ((Get-Caption 103) -eq '未选择窗口') 'legacy_profile_starts_chinese'
    Assert-Smoke ((Get-Caption 121) -eq '恢复全部') 'main_restore_caption_chinese'
    $minimumMain = [TransSmokeNativeV1]::Minimum($root)
    Save-Screenshot '01-main-chinese'
    Click-Control 130
    Assert-Smoke ((Get-Caption 140) -eq '设置') 'settings_caption_chinese'
    $minimumSettings = [TransSmokeNativeV1]::Minimum($root)
    Assert-Smoke ($minimumMain.MinTrackSize.X -eq $minimumSettings.MinTrackSize.X -and $minimumMain.MinTrackSize.Y -eq $minimumSettings.MinTrackSize.Y) 'same_minimum_dimensions' @{ width = $minimumSettings.MinTrackSize.X; height = $minimumSettings.MinTrackSize.Y }
    $initialSettingsBounds = [TransSmokeNativeV1]::Bounds($root)
    [void][TransSmokeNativeV1]::SetWindowPos($root, [IntPtr]::Zero, 0, 0, $initialSettingsBounds.Width, [Math]::Max($initialSettingsBounds.Height, 860), 0x0006)
    Start-Sleep -Milliseconds 180

    $languageCombo = Get-Control 153
    Assert-Smoke ([TransSmokeNativeV1]::Class($languageCombo) -eq 'ComboBox' -and [TransSmokeNativeV1]::Send($languageCombo, 0x0146, 0, 0) -eq 2 -and [TransSmokeNativeV1]::ComboItemText($languageCombo, 0) -eq '中文' -and [TransSmokeNativeV1]::ComboItemText($languageCombo, 1) -eq 'English') 'language_dropdown_has_two_choices'
    $languageBounds = [TransSmokeNativeV1]::Bounds($languageCombo)
    $trayBounds = [TransSmokeNativeV1]::Bounds((Get-Control 142))
    $expectedLanguageHeight = [int][Math]::Round(32 * [TransSmokeNativeV1]::GetDpiForWindow($languageCombo) / 96)
    Assert-Smoke ($languageBounds.Bottom -le $trayBounds.Top -and [Math]::Abs($languageBounds.Height - $expectedLanguageHeight) -le 1) 'language_dropdown_is_first_configuration_row' @{ languageTop = $languageBounds.Top; languageBottom = $languageBounds.Bottom; trayTop = $trayBounds.Top; actualHeight = $languageBounds.Height; expectedHeight = $expectedLanguageHeight }
    Assert-EmptyHotkey '无' $false 'empty_shortcut_chinese_disabled' -CheckBuffer
    [void][TransSmokeNativeV1]::Send($languageCombo, 0x014F, 1, 0)
    Assert-Smoke ([TransSmokeNativeV1]::Send($languageCombo, 0x0157, 0, 0) -eq 1) 'language_dropdown_opens'
    Save-Screenshot '01b-settings-language-dropdown'
    [void][TransSmokeNativeV1]::Send($languageCombo, 0x014F, 0, 0)
    [TransSmokeNativeV1]::Key($languageCombo, 0x28)
    Wait-Smoke { (Get-Caption 140) -eq 'Settings' } 'language keyboard selection applied'
    Assert-Smoke ([TransSmokeNativeV1]::Send($languageCombo, 0x0147, 0, 0) -eq 1 -and [TransSmokeNativeV1]::Send($languageCombo, 0x0157, 0, 0) -eq 0) 'language_dropdown_keyboard_commits_english'
    Assert-Smoke ((Get-Caption 140) -eq 'Settings' -and (Get-Caption 151) -eq 'General' -and (Get-Caption 146) -eq 'Exit Trans') 'switch_english_immediately'
    Assert-Smoke (([IO.File]::ReadAllText($settingsPath)) -match '(?m)^language=en\r?$') 'english_language_saved'
    Assert-EmptyHotkey 'None' $false 'empty_shortcut_english_disabled' -CheckBuffer
    $emptyHotkeyBounds = [TransSmokeNativeV1]::Bounds((Get-Control 148))
    $emptyPanel = [TransSmokeNativeV1]::FindClass($root, 'Trans.SettingsPanel.v1')
    $emptyPanelBounds = [TransSmokeNativeV1]::Bounds($emptyPanel)
    Assert-Smoke ($emptyHotkeyBounds.Top -ge $emptyPanelBounds.Top -and $emptyHotkeyBounds.Bottom -le $emptyPanelBounds.Bottom) 'empty_english_shortcut_visible_for_visual_review'
    Save-Screenshot '01c-settings-english-empty-shortcut-disabled'
    [TransSmokeNativeV1]::Key($languageCombo, 0x26)
    Wait-Smoke { (Get-Caption 140) -eq '设置' } 'language keyboard selection back to Chinese'
    Assert-Smoke ([TransSmokeNativeV1]::Send($languageCombo, 0x0147, 0, 0) -eq 0) 'language_dropdown_keyboard_commits_chinese'
    Assert-Smoke ((Get-Caption 140) -eq '设置' -and (Get-Caption 151) -eq '基础配置') 'switch_chinese_immediately'
    Assert-Smoke (([IO.File]::ReadAllText($settingsPath)) -match '(?m)^language=zh-CN\r?$') 'chinese_language_saved'
    Assert-EmptyHotkey '无' $false 'empty_shortcut_switches_back_chinese'

    [TransSmokeNativeV1]::SetText((Get-Control 180), '17')
    $initialEnabled = [TransSmokeNativeV1]::Send((Get-Control 150), 0x00F0, 0, 0)
    Assert-Smoke ($initialEnabled -eq 0) 'fixture_shortcut_disabled'
    Click-Control 150
    Assert-EmptyHotkey '无' $true 'empty_shortcut_chinese_enabled'
    Select-Language 1
    Assert-EmptyHotkey 'None' $true 'empty_shortcut_english_enabled'
    Save-Screenshot '01d-settings-english-empty-shortcut-enabled'
    Select-Language 0
    Assert-EmptyHotkey '无' $true 'empty_shortcut_enabled_switches_back_chinese'
    [void][TransSmokeNativeV1]::Send((Get-Control 148), 0x0401, 0x0354, 0)
    $draftHotkey = [TransSmokeNativeV1]::Send((Get-Control 148), 0x0402, 0, 0)
    Assert-Smoke ($draftHotkey -eq 0x0354) 'native_hotkey_draft_set'
    Select-Language 1
    Assert-Smoke ((Get-Caption 180) -eq '17') 'language_switch_preserves_preset_draft'
    Assert-Smoke ([TransSmokeNativeV1]::Send((Get-Control 148), 0x0402, 0, 0) -eq $draftHotkey) 'language_switch_preserves_hotkey_draft'
    Assert-Smoke ([TransSmokeNativeV1]::Send((Get-Control 150), 0x00F0, 0, 0) -eq 1) 'language_switch_preserves_enabled_draft'
    $beforePresetSave = [IO.File]::ReadAllText($settingsPath)
    Assert-Smoke ($beforePresetSave -match '(?m)^preset_1=0\r?$' -and $beforePresetSave -match '(?m)^restore_hotkey=None\r?$') 'language_switch_does_not_save_uncommitted_drafts'

    $largeBounds = [TransSmokeNativeV1]::Bounds($root)
    [void][TransSmokeNativeV1]::SetWindowPos($root, [IntPtr]::Zero, 0, 0, $largeBounds.Width, [Math]::Max($largeBounds.Height, 780), 0x0006)
    Start-Sleep -Milliseconds 180
    Save-Screenshot '02-settings-english-expanded'
    $enabledBounds = [TransSmokeNativeV1]::Bounds((Get-Control 150))
    $presetsSaveBounds = [TransSmokeNativeV1]::Bounds((Get-Control 161))
    Assert-Smoke ([Math]::Abs($enabledBounds.Right - $presetsSaveBounds.Right) -le 4) 'enabled_checkbox_aligns_with_presets_save' @{ enabledRight = $enabledBounds.Right; presetsSaveRight = $presetsSaveBounds.Right; tolerancePixels = 4; reason = 'The custom button border is inset from its HWND boundary' }
    [void][TransSmokeNativeV1]::SetWindowPos($root, [IntPtr]::Zero, 0, 0, $minimumSettings.MinTrackSize.X, $minimumSettings.MinTrackSize.Y, 0x0006)
    Start-Sleep -Milliseconds 180
    $minimumActualBounds = [TransSmokeNativeV1]::Bounds($root)
    Assert-Smoke ($minimumActualBounds.Width -eq $minimumSettings.MinTrackSize.X -and $minimumActualBounds.Height -eq $minimumSettings.MinTrackSize.Y) 'physical_minimum_dimensions_applied' @{ expectedWidth = $minimumSettings.MinTrackSize.X; expectedHeight = $minimumSettings.MinTrackSize.Y; actualWidth = $minimumActualBounds.Width; actualHeight = $minimumActualBounds.Height }
    $panel = [TransSmokeNativeV1]::FindClass($root, 'Trans.SettingsPanel.v1')
    Assert-Smoke ($panel -ne [IntPtr]::Zero) 'settings_scroll_panel_present'
    $beforeWheel = [TransSmokeNativeV1]::Bounds((Get-Control 151))
    [void][TransSmokeNativeV1]::Send($panel, 0x020A, 4287102976, 0)
    Wait-Smoke { [TransSmokeNativeV1]::Bounds((Get-Control 151)).Top -lt $beforeWheel.Top } 'settings wheel scroll'
    $afterWheel = [TransSmokeNativeV1]::Bounds((Get-Control 151))
    Assert-Smoke ($afterWheel.Top -lt $beforeWheel.Top) 'minimum_settings_wheel_scrolls_content' @{ beforeTop = $beforeWheel.Top; afterTop = $afterWheel.Top }
    Save-Screenshot '03-settings-english-minimum-scrolled'
    [void][TransSmokeNativeV1]::Send($panel, 0x020A, 4287102976, 0)
    [void][TransSmokeNativeV1]::Send($panel, 0x020A, 4287102976, 0)
    Wait-Smoke {
        $viewportBounds = [TransSmokeNativeV1]::Bounds($panel)
        $hotkeyBounds = [TransSmokeNativeV1]::Bounds((Get-Control 148))
        $hotkeyBounds.Top -ge $viewportBounds.Top -and $hotkeyBounds.Bottom -le $viewportBounds.Bottom
    } 'hotkey row enters minimum settings viewport'
    $viewportBounds = [TransSmokeNativeV1]::Bounds($panel)
    $hotkeyBounds = [TransSmokeNativeV1]::Bounds((Get-Control 148))
    Assert-Smoke ($hotkeyBounds.Top -ge $viewportBounds.Top -and $hotkeyBounds.Bottom -le $viewportBounds.Bottom) 'minimum_settings_hotkey_input_fully_visible' @{ panelTop = $viewportBounds.Top; panelBottom = $viewportBounds.Bottom; inputTop = $hotkeyBounds.Top; inputBottom = $hotkeyBounds.Bottom }
    $hotkeyRowVisible = $true
    $hotkeyRowBounds = @()
    foreach ($rowControlId in @(147, 148, 149)) {
        $rowControl = Get-Control $rowControlId
        $rowBounds = [TransSmokeNativeV1]::Bounds($rowControl)
        $isFullyVisible = [TransSmokeNativeV1]::IsWindowVisible($rowControl) -and $rowBounds.Left -ge $viewportBounds.Left -and $rowBounds.Right -le $viewportBounds.Right -and $rowBounds.Top -ge $viewportBounds.Top -and $rowBounds.Bottom -le $viewportBounds.Bottom
        $hotkeyRowVisible = $hotkeyRowVisible -and $isFullyVisible
        $hotkeyRowBounds += [pscustomobject]@{ controlId = $rowControlId; fullyVisible = $isFullyVisible; left = $rowBounds.Left; right = $rowBounds.Right; top = $rowBounds.Top; bottom = $rowBounds.Bottom }
    }
    Assert-Smoke $hotkeyRowVisible 'minimum_settings_hotkey_row_fully_visible' $hotkeyRowBounds
    Save-Screenshot '03b-settings-english-minimum-hotkey'
    $hotkeyText = 'Ctrl + Shift + T'
    $hotkeyMeasure = [TransSmokeNativeV1]::MeasureControlText((Get-Control 148), $hotkeyText)
    $hotkeyClient = [TransSmokeNativeV1]::ClientBounds((Get-Control 148))
    Assert-Smoke ($hotkeyClient.Width -ge $hotkeyMeasure.Width + 2) 'minimum_settings_hotkey_text_fits_client_width' @{ text = $hotkeyText; availableWidth = $hotkeyClient.Width; measuredWidth = $hotkeyMeasure.Width; fontFace = $hotkeyMeasure.FontFace; fontHeight = $hotkeyMeasure.FontHeight; caretAllowance = 2 }

    Move-Item -LiteralPath $settingsPath -Destination $backupPath
    New-Item -ItemType Directory -Path $settingsPath | Out-Null
    $faultDirectoryCreated = $true
    Select-Language 0 -Async
    Wait-Smoke { [TransSmokeNativeV1]::FindDialog($testProcess.Id) -ne [IntPtr]::Zero } 'save failure warning dialog'
    $dialog = [TransSmokeNativeV1]::FindDialog($testProcess.Id)
    Record-Dialog 'before-confirmation' $dialog
    $dialogText = [string]::Join("`n", [TransSmokeNativeV1]::ChildTexts($dialog))
    Assert-Smoke ($dialogText -match 'Could not replace settings file') 'language_save_failure_reports_error' $dialogText
    Assert-Smoke ((Get-Caption 140) -eq 'Settings' -and (Get-Caption 151) -eq 'General') 'failed_save_keeps_previous_language'
    Assert-Smoke ([TransSmokeNativeV1]::Send((Get-Control 153), 0x0147, 0, 0) -eq 1) 'failed_language_save_rolls_dropdown_back_to_english'
    [void][TransSmokeNativeV1]::Send($dialog, 0x0111, 1, [TransSmokeNativeV1]::Control($dialog, 1).ToInt64())
    Start-Sleep -Milliseconds 200
    Record-Dialog 'after-WM_COMMAND-IDOK' $dialog
    $remainingDialog = [TransSmokeNativeV1]::FindDialog($testProcess.Id)
    if ($remainingDialog -ne [IntPtr]::Zero) {
        if ($remainingDialog -ne $dialog) {
            throw 'A different warning dialog appeared after IDOK; inspect dialog-diagnostics.json'
        }
        [void][TransSmokeNativeV1]::PostMessage($remainingDialog, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero)
        Start-Sleep -Milliseconds 200
        Record-Dialog 'after-owned-WM_CLOSE' $dialog
    }
    try {
        Wait-Smoke { [TransSmokeNativeV1]::FindDialog($testProcess.Id) -eq [IntPtr]::Zero } 'close owned warning dialog'
    } catch {
        Record-Dialog 'close-timeout' $dialog
        throw
    }
    Remove-Item -LiteralPath $settingsPath
    $faultDirectoryCreated = $false
    Move-Item -LiteralPath $backupPath -Destination $settingsPath
    Assert-Smoke ((Get-Caption 180) -eq '17') 'failed_save_preserves_preset_draft'
    Assert-Smoke ([TransSmokeNativeV1]::Send((Get-Control 148), 0x0402, 0, 0) -eq $draftHotkey) 'failed_save_preserves_hotkey_draft'
    Click-Control 161
    $saved = [IO.File]::ReadAllText($settingsPath)
    Assert-Smoke ($saved -match '(?m)^preset_1=17\r?$' -and $saved -match '(?m)^language=en\r?$') 'preset_save_persists_with_english'
    Click-Control 141
    Assert-Smoke ((Get-Caption 103) -eq 'No selection' -and (Get-Caption 121) -eq 'Restore all') 'main_page_captions_english'
    Assert-Smoke ((Get-Caption 110) -eq '17%') 'preset_button_updated'
    Save-Screenshot '04-main-english-minimum'
    Click-Control 130
    Stop-TestApplication
    Assert-Smoke ($testProcess.HasExited) 'exit_closes_isolated_process'

    Start-TestApplication
    Assert-Smoke ((Get-Caption 103) -eq 'No selection') 'english_restored_after_restart'
    Assert-Smoke ((Get-Caption 110) -eq '17%') 'preset_restored_after_restart'
    Click-Control 130
    Assert-Smoke ((Get-Caption 140) -eq 'Settings') 'settings_english_restored_after_restart'
    Assert-Smoke ([TransSmokeNativeV1]::Send((Get-Control 150), 0x00F0, 0, 0) -eq 0) 'unsaved_hotkey_draft_not_persisted'
    Assert-Smoke ([TransSmokeNativeV1]::Send((Get-Control 148), 0x0402, 0, 0) -eq 0) 'shortcut_remains_disabled_after_restart'
    Assert-EmptyHotkey 'None' $false 'empty_shortcut_english_after_restart'
    Save-Screenshot '05-settings-english-restarted'
    Stop-TestApplication
}
catch {
    $failure = $_.Exception.ToString()
    if ($root -ne [IntPtr]::Zero -and [TransSmokeNativeV1]::IsWindow($root)) {
        try { Save-Screenshot 'failure' } catch { }
    }
}
finally {
    if ($faultDirectoryCreated -and (Test-Path -LiteralPath $settingsPath -PathType Container)) {
        Remove-Item -LiteralPath $settingsPath
    }
    if ((Test-Path -LiteralPath $backupPath -PathType Leaf) -and -not (Test-Path -LiteralPath $settingsPath)) {
        Move-Item -LiteralPath $backupPath -Destination $settingsPath
    }
    try { Stop-TestApplication } catch { if ($null -eq $failure) { $failure = $_.Exception.ToString() } }
    $report = [pscustomobject]@{
        executable = $exeFullPath
        profile = $profilePath
        completedAt = [DateTime]::UtcNow.ToString('o')
        passed = ($null -eq $failure)
        checks = $checks.ToArray()
        screenshots = $shots.ToArray()
        dialogDiagnostics = $dialogDiagnostics.ToArray()
        error = $failure
        limits = @('Native message and PrintWindow smoke checks; no physical mouse or keyboard input', 'No test of other applications or real user settings', 'Target-window transparency is outside this script')
    }
    $jsonPath = Join-Path $artifactFullPath 'win32-smoke-results.json'
    [IO.File]::WriteAllText($jsonPath, ($report | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    $report | ConvertTo-Json -Depth 8
    [void][TransSmokeNativeV1]::SetThreadDpiAwarenessContext($previousDpiContext)
}
if ($null -ne $failure) { throw $failure }
