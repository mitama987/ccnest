# Real Windows Terminal check for "extend the selection to the click" (v0.1.44):
# does an Alt+click (and a Shift+click) made with the real pointer reach ccnest?
#
# click-extend-selection.ps1 writes console input records directly, which skips
# Windows Terminal's own pointer handling. WT decides there whether a click goes to
# the app (VT mouse) or to WT's own selection, so this one script uses real input
# (SetCursorPos + mouse_event + keybd_event) on a throwaway WT window:
#   1. plain click            (start point)
#   2. Alt+click lower down   -> expect `Mouse(Down(Left)@..,KeyModifiers(ALT))` and
#                                `extend_selection` in CCNEST_INPUT_TRACE
#   3. Shift+click            -> expect NO mouse event with SHIFT (WT keeps Shift+click
#                                for its own selection and never sends it to the app)
# and saves a screenshot after step 2 (PrintWindow).
#
# Real input moves the pointer and takes the focus for about 2 seconds, so the script
# waits until the user has been idle for -IdleSec seconds (up to -WaitSec) and gives
# up without clicking if that never happens. It only clicks after checking that the
# throwaway window is what is under both click points. Pointer position and the
# foreground window are put back afterwards. The user's ccnest is never touched.
# ASCII only (no BOM needed).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-click-extend-wt'),
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
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
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
  public static void Click(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(80);
    mouse_event(0x2, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(80);
    mouse_event(0x4, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(350);
  }
  public static void ModClick(byte vk, int x, int y) {
    keybd_event(vk, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(80);
    try { Click(x, y); } finally { keybd_event(vk, 0, 0x2, UIntPtr.Zero); }
    System.Threading.Thread.Sleep(150);
  }
}
"@
Add-Type -AssemblyName System.Drawing
[void][W]::SetProcessDPIAware()

$tag = 'ccnest-extend-' + (Get-Date -Format 'HHmmss')
$work = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\target')).Path $tag
New-Item -ItemType Directory -Force $work | Out-Null
$exe = (Resolve-Path $Exe).Path
$shimPs = Join-Path $PSScriptRoot 'click-extend-selection-shim.ps1'
$shim = Join-Path $work 'claude-shim.cmd'
Set-Content $shim "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`"" -Encoding ASCII
# WT does not always hand its environment to the new window, so set it inside.
$run = Join-Path $work 'run.cmd'
Set-Content $run @('@echo off', 'set CCNEST_INPUT_TRACE=1', 'set CCNEST_JP_TITLE=off', "set CCNEST_CLAUDE_BIN=$shim", "`"$exe`"") -Encoding ASCII
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$traceFrom = if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 }
$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })

function Shot($h, $name) {
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($h, [ref]$r)
  $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($bmp)
  $hdc = $g.GetHdc()
  [void][W]::PrintWindow($h, $hdc, 2)
  $g.ReleaseHdc($hdc)
  $p = Join-Path $OutDir $name
  $bmp.Save($p, [Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  "shot: $p ($((Get-Item $p).Length) bytes)"
}
function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function NewTrace { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $traceFrom) } else { @() } }

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
  Start-Sleep -Seconds 6
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($hwnd, [ref]$r)
  $h = $r.B - $r.T
  # Three points inside the pane: start, lower (Alt), lower still (Shift).
  $ax = $r.L + 320; $ay = $r.T + [int]($h * 0.42)
  $bx = $r.L + 620; $by = $r.T + [int]($h * 0.62)
  $cx = $r.L + 420; $cy = $r.T + [int]($h * 0.74)

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
    $covered = @(@($ax, $ay), @($bx, $by), @($cx, $cy) | Where-Object { [W]::RootAt($_[0], $_[1]) -ne $hwnd })
    if ($covered.Count -gt 0) {
      "SKIPPED: another window is over the click points. No real input was sent."
      $result = 'SKIPPED'
    } else {
      $pos = New-Object W+POINT
      [void][W]::GetCursorPos([ref]$pos)
      $fg = [W]::GetForegroundWindow()
      try {
        "real input: idle was $([Math]::Round($idle, 1)) s; click ($ax,$ay), Alt+click ($bx,$by), Shift+click ($cx,$cy)"
        [W]::Click($ax, $ay)      # first click may only activate the window
        [W]::Click($ax, $ay)      # plain click = start point
        [W]::ModClick(0x12, $bx, $by)   # VK_MENU
        Start-Sleep -Milliseconds 400
        Shot $hwnd 'alt-click.png'
        [W]::ModClick(0x10, $cx, $cy)   # VK_SHIFT
        Start-Sleep -Milliseconds 400
        Shot $hwnd 'shift-click.png'
      } finally {
        [void][W]::SetCursorPos($pos.X, $pos.Y)
        if ($fg -ne [IntPtr]::Zero) { [void][W]::SetForegroundWindow($fg) }
      }
      Start-Sleep -Milliseconds 600
      $new = NewTrace
      $mouse = @($new | ForEach-Object { [regex]::Matches($_, 'Mouse\((Down|Up)\(Left\)@[^)]*\)+') | ForEach-Object { $_.Value } })
      "mouse events ccnest received:"
      $mouse | ForEach-Object { "  $_" }
      $ext = @($new | Where-Object { $_ -match 'extend_selection' } | ForEach-Object { $_ -replace '^\S+\s+', '' })
      $ext | ForEach-Object { "  $_" }
      $altDown = @($mouse | Where-Object { $_ -match 'Down\(Left\).*ALT' }).Count
      $shiftAny = @($mouse | Where-Object { $_ -match 'SHIFT' }).Count
      $fails = 0
      if ($altDown -ge 1) { "  ok   Alt+click reached ccnest with the ALT modifier" } else { "  FAIL no Down(Left) with ALT"; $fails++ }
      if ($ext.Count -ge 1) { "  ok   ccnest extended the selection ($($ext.Count) line)" } else { "  FAIL no extend_selection line"; $fails++ }
      if ($shiftAny -eq 0) { "  info Shift+click did not reach ccnest (Windows Terminal kept it for its own selection)" } else { "  info Shift+click reached ccnest ($shiftAny event(s) with SHIFT)" }
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
