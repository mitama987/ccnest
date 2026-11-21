# Real Windows Terminal check for "double-click a tab to rename it" (v0.1.45):
# does a double-click made with the real pointer reach ccnest as two Down(Left) events,
# and how does the input look in WT?
#
# dblclick-rename-tab.ps1 writes console input records directly, which skips Windows
# Terminal's own pointer handling (WT decides there whether a click goes to the app or
# to WT's own selection). So this one script uses real input (SetCursorPos + mouse_event)
# on a throwaway WT window:
#   1. two probe clicks inside the pane -> CCNEST_INPUT_TRACE reports their cells, which
#      gives the cell size in pixels (no guessing where the tab bar is)
#   2. a real double-click on tab 1 -> expect two Down(Left)@..,0 and `begin_rename_tab idx=0`
#   3. screenshot (the name is selected), then type + Enter through the console input
#      (keyboard input is not what is being checked here, and real keys would go through
#      the IME) and screenshot the renamed tab
#
# Real input moves the pointer and takes the focus for about 2 seconds, so the script
# waits until the user has been idle for -IdleSec seconds (up to -WaitSec) and gives up
# without clicking if that never happens. It only clicks after checking that the
# throwaway window is what is under the click points. Pointer position and the foreground
# window are put back afterwards. The user's ccnest is never touched, and CCNEST_TAB_NAMES
# points at a temp file. ASCII only (no BOM needed).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-dblclick-rename-wt'),
  [int]$IdleSec = 4, [int]$WaitSec = 120
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutDir | Out-Null

Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
  [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool GetLastInputInfo(ref LASTINPUTINFO i);
  [DllImport("kernel32.dll")] public static extern uint GetTickCount();
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
  public static IntPtr Find(string title) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { if (!IsWindowVisible(h)) return true; var sb = new StringBuilder(512); GetWindowText(h, sb, 512); if (sb.ToString() == title) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
  public static double IdleSeconds() {
    var i = new LASTINPUTINFO(); i.cbSize = 8;
    GetLastInputInfo(ref i);
    return (GetTickCount() - i.dwTime) / 1000.0;
  }
  public static IntPtr RootAt(int x, int y) { return GetAncestor(WindowFromPoint(new POINT { X = x, Y = y }), 2); }
  static void Press() {
    mouse_event(0x2, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(50);
    mouse_event(0x4, 0, 0, 0, UIntPtr.Zero);
  }
  public static void Click(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(80);
    Press(); System.Threading.Thread.Sleep(700);   // longer than a double-click
  }
  // Two presses 150 ms apart (well inside ccnest's 500 ms).
  public static void DoubleClick(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(80);
    Press(); System.Threading.Thread.Sleep(100);
    Press(); System.Threading.Thread.Sleep(400);
  }
}
"@
Add-Type -AssemblyName System.Drawing
[void][W]::SetProcessDPIAware()

$tag = 'ccnest-dblrename-' + (Get-Date -Format 'HHmmss')
$work = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\target')).Path $tag
New-Item -ItemType Directory -Force $work | Out-Null
$exe = (Resolve-Path $Exe).Path
$shim = Join-Path $work 'claude-shim.cmd'
Set-Content $shim @('@echo off', 'title fix-payment-webhook', 'echo SHIM', 'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600') -Encoding ASCII
# WT does not always hand its environment to the new window, so set it inside.
$run = Join-Path $work 'run.cmd'
Set-Content $run @(
  '@echo off', 'set CCNEST_PID=', 'set CCNEST_INPUT_TRACE=1', 'set CCNEST_JP_TITLE=off', 'set CCNEST_USAGE=off',
  "set CCNEST_TAB_NAMES=$(Join-Path $work 'tab-names.json')", "set CCNEST_CLAUDE_BIN=$shim", "`"$exe`""
) -Encoding ASCII
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$traceFrom = if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 }
$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$screenFile = Join-Path $work 'screen.txt'

# PrintWindow(PW_RENDERFULLCONTENT): works while other windows cover this one. A minimized
# window comes back black, so restore it (without activating) first. Keeps the top 170 px.
function Shot($h, $name) {
  if ([W]::IsIconic($h)) { [void][W]::ShowWindow($h, 4); Start-Sleep -Milliseconds 500 }
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($h, [ref]$r)
  $full = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($full)
  $hdc = $g.GetHdc()
  [void][W]::PrintWindow($h, $hdc, 2)
  $g.ReleaseHdc($hdc); $g.Dispose()
  $ch = [Math]::Min(170, $full.Height)
  $cwid = [Math]::Min(900, $full.Width)
  $crop = $full.Clone((New-Object Drawing.Rectangle 0, 0, $cwid, $ch), $full.PixelFormat)
  $p = Join-Path $OutDir $name
  $crop.Save($p, [Drawing.Imaging.ImageFormat]::Png)
  $crop.Dispose(); $full.Dispose()
  "shot: $p ($((Get-Item $p).Length) bytes)"
}
function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function NewTrace { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $traceFrom) } else { @() } }
# Every Down(Left) cell ccnest has logged since the start, in order: @(col, row), ...
function Downs { @(NewTrace | ForEach-Object { [regex]::Matches($_, 'Mouse\(Down\(Left\)@(\d+),(\d+)') | ForEach-Object { , @([int]$_.Groups[1].Value, [int]$_.Groups[2].Value) } }) }
# Keys through the console input of the throwaway ccnest (inner mode of the conhost script).
function Keys($ccId, $ops) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"") -WindowStyle Hidden -Wait
}

$cc = $null; $hwnd = [IntPtr]::Zero; $result = 'FAIL'
$prevFg = [W]::GetForegroundWindow()
try {
  Start-Process wt.exe -ArgumentList @('-w', 'new', '--size', '100,22', '-d', "`"$work`"", 'cmd.exe', '/c', "`"$run`"")
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $cc = Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1
  } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw 'no new ccnest' }
  "ccnest pid: $($cc.Id)"
  $deadline = (Get-Date).AddSeconds(15)
  do { Start-Sleep -Milliseconds 500; $hwnd = [W]::Find($tag) } while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
  if ($hwnd -eq [IntPtr]::Zero) { throw "window '$tag' not found" }
  [void][W]::MoveWindow($hwnd, 0, 0, 1400, 620, $true)
  # Give the focus back right away; the window only needs it for the clicks.
  if ($prevFg -ne [IntPtr]::Zero) { [void][W]::SetForegroundWindow($prevFg) }
  Start-Sleep -Seconds 7   # the tab label needs a 2 s tick to show the task name
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($hwnd, [ref]$r)
  $h = $r.B - $r.T
  # Two probe points inside the pane (far apart in both directions).
  $ax = $r.L + 500; $ay = $r.T + [int]($h * 0.45)
  $bx = $r.L + 1000; $by = $r.T + [int]($h * 0.80)

  $deadline = (Get-Date).AddSeconds($WaitSec)
  while ([W]::IdleSeconds() -lt $IdleSec -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
  $idle = [W]::IdleSeconds()
  if ($idle -lt $IdleSec) {
    "SKIPPED: the user was never idle for $IdleSec s within $WaitSec s (last idle $([Math]::Round($idle, 1)) s). No real input was sent."
    $result = 'SKIPPED'
  } else {
    # Put the window on top without activating it, and make sure it is what we would click.
    [void][W]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x13)   # HWND_TOPMOST, NOSIZE|NOMOVE|NOACTIVATE
    Start-Sleep -Milliseconds 300
    $covered = @(@($ax, $ay), @($bx, $by) | Where-Object { [W]::RootAt($_[0], $_[1]) -ne $hwnd })
    if ($covered.Count -gt 0) {
      "SKIPPED: another window is over the click points. No real input was sent."
      $result = 'SKIPPED'
    } else {
      $pos = New-Object W+POINT
      [void][W]::GetCursorPos([ref]$pos)
      $fg = [W]::GetForegroundWindow()
      $fails = 0
      try {
        "real input: idle was $([Math]::Round($idle, 1)) s"
        [W]::Click($ax, $ay)      # first click may only activate the window
        [W]::Click($ax, $ay)
        [W]::Click($bx, $by)
        Start-Sleep -Milliseconds 300
        $d = Downs
        if ($d.Count -lt 2) { throw "probe clicks did not reach ccnest ($($d.Count) Down events)" }
        $p1 = $d[$d.Count - 2]; $p2 = $d[$d.Count - 1]
        if ($p2[0] -eq $p1[0] -or $p2[1] -eq $p1[1]) { throw "probe cells too close: $($p1 -join ',') / $($p2 -join ',')" }
        $cw = ($bx - $ax) / ($p2[0] - $p1[0]); $chh = ($by - $ay) / ($p2[1] - $p1[1])
        "probe: ($ax,$ay) = cell $($p1 -join ','), ($bx,$by) = cell $($p2 -join ',') -> cell $([Math]::Round($cw, 1)) x $([Math]::Round($chh, 1)) px"
        # Middle of cell (col 6, row 0): inside the label of tab 1.
        $tx = [int]($ax + (6 - $p1[0]) * $cw); $ty = [int]($ay + (0 - $p1[1]) * $chh)
        if ([W]::RootAt($tx, $ty) -ne $hwnd) { throw "the tab point ($tx,$ty) is not on the throwaway window" }
        $n0 = $d.Count
        "double-click at ($tx,$ty)"
        [W]::DoubleClick($tx, $ty)
      } finally {
        [void][W]::SetCursorPos($pos.X, $pos.Y)
        if ($fg -ne [IntPtr]::Zero) { [void][W]::SetForegroundWindow($fg) }
      }
      Start-Sleep -Milliseconds 600
      $d = Downs
      $got = @($d | Select-Object -Skip $n0)
      "Down(Left) events from the double-click: " + (($got | ForEach-Object { $_ -join ',' }) -join ' / ')
      $onTabBar = @($got | Where-Object { $_[1] -eq 0 }).Count
      if ($onTabBar -eq 2) { "  ok   both presses reached ccnest on the tab bar row" } else { "  FAIL expected 2 Down(Left) on row 0, got $onTabBar"; $fails++ }
      $begin = @(NewTrace | Where-Object { $_ -match 'begin_rename_tab idx=0' }).Count
      if ($begin -eq 1) { "  ok   ccnest opened the name input (begin_rename_tab idx=0)" } else { "  FAIL begin_rename_tab idx=0 lines: $begin"; $fails++ }
      Shot $hwnd '1-name-selected.png'
      Keys $cc.Id 'type:pay'
      Shot $hwnd '2-typing.png'
      Keys $cc.Id 'enter'
      Start-Sleep -Milliseconds 300
      Shot $hwnd '3-renamed.png'
      $named = @(NewTrace | Where-Object { $_ -match '[^_]rename_tab idx=0 name=Some\("pay"\)' }).Count
      if ($named -eq 1) { "  ok   renamed (rename_tab idx=0 name=Some(`"pay`"))" } else { "  FAIL rename_tab lines: $named"; $fails++ }
      if ($fails -eq 0) { $result = 'PASS' }
    }
  }
} finally {
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::SetWindowPos($hwnd, [IntPtr](-2), 0, 0, 0, 0, 0x13) }
  if ($cc) { Kill-Tree $cc.Id }
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
  Start-Sleep -Seconds 1
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
$result
if ($result -eq 'FAIL') { exit 1 }
