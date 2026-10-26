# Test our windows without focus: `. tools\shot.ps1`, then Start-Quiet, Get-Shot (GLINT_SHOT_DIR, crates/ui/src/window/shot.rs), Send-Click / Click-Element / Send-Text / Send-Key, Stop-Quiet.

# Only posted messages reach the window. UI Automation is used to read where things are, never to
# act: its Invoke / Toggle / SetValue bring the window to the front.
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
if (-not ('GlintShot.Win' -as [type])) {
    Add-Type -Namespace GlintShot -Name Win -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern uint RegisterWindowMessageW(string name);
[DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern ushort GlobalAddAtomW(string name);
[DllImport("kernel32.dll")] public static extern ushort GlobalDeleteAtom(ushort atom);
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr wp, IntPtr lp);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
[DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr hwnd, ref POINT p);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
[DllImport("user32.dll")] public static extern short VkKeyScanW(char c);
[DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code, uint type);
public delegate bool EnumProc(IntPtr hwnd, IntPtr lp);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc proc, IntPtr lp);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr hwnd, System.Text.StringBuilder s, int n);
public struct RECT { public int Left, Top, Right, Bottom; }
public struct POINT { public int X, Y; }
public static System.Collections.Generic.List<IntPtr> WindowsOf(uint pid) {
    var found = new System.Collections.Generic.List<IntPtr>();
    EnumWindows((h, _) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid && IsWindowVisible(h)) found.Add(h); return true; }, IntPtr.Zero);
    return found;
}
// One key pressed and let go, in one go: winit takes a WM_CHAR only when it's already queued
// right behind its WM_KEYDOWN.
public static void PostKey(IntPtr hwnd, uint vk, char c) {
    long scan = MapVirtualKeyW(vk, 0) << 16;
    PostMessageW(hwnd, 0x0100, (IntPtr)vk, (IntPtr)(1 | scan));                              // WM_KEYDOWN
    if (c != '\0') PostMessageW(hwnd, 0x0102, (IntPtr)c, (IntPtr)(1 | scan));               // WM_CHAR
    PostMessageW(hwnd, 0x0101, (IntPtr)vk, (IntPtr)(1 | scan | 0xC0000000L));              // WM_KEYUP
}
'@
}
$script:ShotMessage = [GlintShot.Win]::RegisterWindowMessageW('glint-shot')
$script:DropMessage = [GlintShot.Win]::RegisterWindowMessageW('glint-drop')

# Starts `exe` with `arguments` quietly (GLINT_QUIET=1), shots going to `shots`, with its own
# APPDATA and LOCALAPPDATA when given; waits for its window titled `title` and returns
# @{ Process; Hwnd }. `back` sends the window behind everything (it still renders and shoots).
function Start-Quiet([string]$exe, [string]$arguments, [string]$title, [string]$shots, [string]$appdata, [string]$local, [switch]$back, [int]$timeout = 20) {
    $si = New-Object System.Diagnostics.ProcessStartInfo $exe, $arguments
    $si.UseShellExecute = $false
    $si.EnvironmentVariables['GLINT_QUIET'] = '1'
    $si.EnvironmentVariables['GLINT_SHOT_DIR'] = $shots
    if ($appdata) { $si.EnvironmentVariables['APPDATA'] = $appdata }
    if ($local) { $si.EnvironmentVariables['LOCALAPPDATA'] = $local }
    New-Item -ItemType Directory -Force $shots | Out-Null
    $p = [System.Diagnostics.Process]::Start($si)
    $deadline = (Get-Date).AddSeconds($timeout)
    while ((Get-Date) -lt $deadline -and -not $p.HasExited) {
        foreach ($h in [GlintShot.Win]::WindowsOf([uint32]$p.Id)) {
            $sb = New-Object System.Text.StringBuilder 256
            [void][GlintShot.Win]::GetWindowTextW($h, $sb, 256)
            if ($sb.ToString() -eq $title) {
                if ($back) { Send-Back $h }
                return [pscustomobject]@{ Process = $p; Hwnd = $h }
            }
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $p.HasExited) { Stop-Process -Id $p.Id }
    throw "no window '$title' from $exe"
}

# Puts the window at the bottom of the z-order, without activating it.
function Send-Back([IntPtr]$hwnd) {
    # HWND_BOTTOM; SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE
    [void][GlintShot.Win]::SetWindowPos($hwnd, [IntPtr]1, 0, 0, 0, 0, 0x13)
}

# Throws once the window is in front: whatever came last took the focus.
function Assert-Background([IntPtr]$hwnd) {
    if ([GlintShot.Win]::GetForegroundWindow() -eq $hwnd) { throw "the window $hwnd came to the front" }
}

# Asks the window for `<shots>\<name>.png` and waits for it; returns its path.
function Get-Shot([IntPtr]$hwnd, [string]$shots, [string]$name, [int]$timeout = 10) {
    $path = Join-Path $shots "$name.png"
    Remove-Item -ErrorAction SilentlyContinue $path
    $atom = [GlintShot.Win]::GlobalAddAtomW($name)
    if (-not [GlintShot.Win]::PostMessageW($hwnd, $script:ShotMessage, [IntPtr][int]$atom, [IntPtr]::Zero)) {
        [void][GlintShot.Win]::GlobalDeleteAtom($atom)
        throw "can't post to $hwnd"
    }
    $deadline = (Get-Date).AddSeconds($timeout)
    while (-not (Test-Path $path)) {
        if ((Get-Date) -gt $deadline) { throw "no shot $name" }
        Start-Sleep -Milliseconds 50
    }
    Assert-Background $hwnd
    $path
}

function Get-Lparam([int]$x, [int]$y) { [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)) }

# The pointer over client pixel (x, y), posted.
function Send-Move([IntPtr]$hwnd, [int]$x, [int]$y) {
    [void][GlintShot.Win]::PostMessageW($hwnd, 0x0200, [IntPtr]::Zero, (Get-Lparam $x $y)) # WM_MOUSEMOVE
}

# A left click at client pixel (x, y), posted: no focus, no cursor.
function Send-Click([IntPtr]$hwnd, [int]$x, [int]$y) {
    $lp = Get-Lparam $x $y
    # a frame for each, as a hand would give them; the last move (one pixel over: winit drops one
    # that goes nowhere) gives a focus asked for its frame to take hold
    Send-Move $hwnd $x $y
    Start-Sleep -Milliseconds 40
    [void][GlintShot.Win]::PostMessageW($hwnd, 0x0201, [IntPtr]1, $lp)      # WM_LBUTTONDOWN, MK_LBUTTON
    Start-Sleep -Milliseconds 40
    [void][GlintShot.Win]::PostMessageW($hwnd, 0x0202, [IntPtr]::Zero, $lp) # WM_LBUTTONUP
    Start-Sleep -Milliseconds 40
    Send-Move $hwnd ($x + 1) $y
    Start-Sleep -Milliseconds 40
}

# The wheel turned by `notches` (positive: away, scrolling up) at client pixel (x, y), posted.
function Send-Wheel([IntPtr]$hwnd, [int]$x, [int]$y, [int]$notches) {
    $r = New-Object GlintShot.Win+RECT
    [void][GlintShot.Win]::GetWindowRect($hwnd, [ref]$r)
    # WM_MOUSEWHEEL carries screen coordinates
    $wp = [IntPtr](([int16]($notches * 120)) * 65536)
    [void][GlintShot.Win]::PostMessageW($hwnd, 0x020A, $wp, (Get-Lparam ($r.Left + $x) ($r.Top + $y)))
}

# A file dragged over the window without the cursor (GLINT_SHOT_DIR windows only): `hold` it
# over the window, `drop` it, or take it away (`leave`), as winit reports a real drag to egui.
function Send-Drop([IntPtr]$hwnd, [string]$path, [ValidateSet('hold', 'drop', 'leave')][string]$what = 'drop') {
    $atom = [GlintShot.Win]::GlobalAddAtomW($path)
    $lp = [IntPtr](@{ hold = 0; drop = 1; leave = 2 }[$what])
    if (-not [GlintShot.Win]::PostMessageW($hwnd, $script:DropMessage, [IntPtr][int]$atom, $lp)) {
        [void][GlintShot.Win]::GlobalDeleteAtom($atom)
        throw "can't post to $hwnd"
    }
}

# The first element in the window named `name` (and of control `type`, e.g. 'Edit', 'Button'),
# read through UI Automation.
function Find-Element([IntPtr]$hwnd, [string]$name, [string]$type, [int]$timeout = 5) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $conds = @()
    if ($name) { $conds += New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::NameProperty), $name }
    if ($type) { $conds += New-Object System.Windows.Automation.PropertyCondition ([System.Windows.Automation.AutomationElement]::ControlTypeProperty), ([System.Windows.Automation.ControlType]::$type) }
    $cond = if ($conds.Count -eq 1) { $conds[0] } else { New-Object System.Windows.Automation.AndCondition (, [System.Windows.Automation.Condition[]]$conds) }
    $deadline = (Get-Date).AddSeconds($timeout)
    do {
        # egui builds its accessibility tree once asked, a frame later
        $e = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
        if ($e -and -not $e.Current.BoundingRectangle.IsEmpty) { return $e }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "no element '$name' $type"
}

# A posted click on the middle of the element `name` (see Find-Element).
function Click-Element([IntPtr]$hwnd, [string]$name, [string]$type) {
    $b = (Find-Element $hwnd $name $type).Current.BoundingRectangle
    $p = New-Object GlintShot.Win+POINT
    $p.X = [int]($b.X + $b.Width / 2); $p.Y = [int]($b.Y + $b.Height / 2)
    [void][GlintShot.Win]::ScreenToClient($hwnd, [ref]$p)
    Send-Click $hwnd $p.X $p.Y
}

# A key pressed and let go, posted: `vk` a virtual-key code, `char` what it types (0: nothing).
function Send-Key([IntPtr]$hwnd, [int]$vk, [int]$char = 0) {
    [GlintShot.Win]::PostKey($hwnd, $vk, [char]$char)
}

# Types `text` into whatever has egui's focus (click the field first), posted.
function Send-Text([IntPtr]$hwnd, [string]$text) {
    foreach ($c in $text.ToCharArray()) {
        $vk = [GlintShot.Win]::VkKeyScanW($c) -band 0xFF
        if ($vk -eq 0xFF) { $vk = 0 }
        Send-Key $hwnd $vk ([int]$c)
    }
}

# Backspace `n` times, posted.
function Send-Erase([IntPtr]$hwnd, [int]$n = 1) {
    for ($i = 0; $i -lt $n; $i++) { Send-Key $hwnd 0x08 8 }
}

# Ends a window this script started, by its PID only.
function Stop-Quiet($w) {
    if ($w -and -not $w.Process.HasExited) { Stop-Process -Id $w.Process.Id }
}
