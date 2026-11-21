# Working (green) tabs are parked small on the right, tabs that need you stay named on the
# left (v0.1.47) - check on the real binary.
# Starts a throw-away ccnest in conhost with a fake claude per tab:
#   1 alpha = idle (needs you)      2 bravo = working      3 cc = working
#   4 delta = idle                  5 echo  = working
# "working" = the pane shows "esc to interrupt" (ccnest reads that as Busy) until the file
# idle<N> appears in the work folder; then the fake claude clears the screen (= needs you).
# Keys and mouse go in through WriteConsoleInputW (inner mode of dblclick-rename-tab.ps1),
# so this never collides with the user's keyboard and mouse. The user's ccnest is not touched.
#
#  A. after the grace: named tabs alpha, delta on the left, a divider, then small 2, 3 and
#     the active tab echo (parked too, but the active tab keeps its name)
#  B. click alpha -> echo becomes small as well (2, 3, 5); nothing else moves
#  C. click small 3 -> it shows its name in place (same column), 2 and 5 stay small
#  D. double-click small 2 -> the name input opens on tab 2 (the tab did not jump away)
#  E. cc stops working -> it moves back to the left group, in tab order (alpha, cc, delta)
#  F. drag alpha onto cc (bravo sits between them in tab order) -> one move; drag alpha far
#     right over the parked group -> it stops at the end of the named group
#  G. Alt+Right from the last named tab goes to the first parked tab (tab-bar order)
# -Off: CCNEST_TAB_GROUP=off -> all five tabs named, in tab order, no divider.
# -Wt: same start in a Windows Terminal window, screenshots of A and B (PrintWindow).
# ASCII only (no BOM needed).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [switch]$Off, [switch]$Wt,
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-park-tabs-shots')
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$tag = 'ccnest-e2e-park-tabs-' + (Get-Date -Format 'HHmmss')
$work = Join-Path $env:TEMP $tag
New-Item -ItemType Directory -Force $work | Out-Null
$counter = Join-Path $work 'n.txt'
$shim = Join-Path $work 'claude-shim.cmd'
Set-Content $shim @(
  '@echo off',
  'setlocal',
  'set N=0',
  "if exist `"$counter`" set /p N=<`"$counter`"",
  'set /a N=N+1',
  ">`"$counter`" echo %N%",
  'set T=other%N%',
  'if %N%==1 set T=alpha',
  'if %N%==2 set T=bravo',
  'if %N%==3 set T=cc',
  'if %N%==4 set T=delta',
  'if %N%==5 set T=echo',
  'title %T%',
  'if %N%==2 goto busy',
  'if %N%==3 goto busy',
  'if %N%==5 goto busy',
  'echo SHIM %N% idle',
  'goto wait',
  ':busy',
  'echo SHIM %N% working - esc to interrupt',
  ':bw',
  "if exist `"$work\idle%N%`" goto nowidle",
  'ping -n 2 127.0.0.1 >nul',
  'goto bw',
  ':nowidle',
  'cls',
  'echo SHIM %N% done',
  ':wait',
  'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600'
) -Encoding ASCII
$run = Join-Path $work 'run.cmd'
$runLines = @(
  '@echo off', 'set CCNEST_PID=', 'set CCNEST_JP_TITLE=off', 'set CCNEST_USAGE=off', 'set CCNEST_INPUT_TRACE=1',
  "set CCNEST_TAB_NAMES=$(Join-Path $work 'tab-names.json')", "set CCNEST_CLAUDE_BIN=$shim"
)
if ($Off) { $runLines += 'set CCNEST_TAB_GROUP=off' }
$runLines += "`"$exe`""
Set-Content $run $runLines -Encoding ASCII
$screenFile = Join-Path $work 'screen.txt'
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$DIV = [string][char]0x2502   # the divider between the two groups

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Send($ccId, $ops) {
  if (-not $ops) { $ops = 'sleep:1' }
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"") -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 2) { throw "inner failed: $($l -join ' ')" }
  @{ Row = $l[0]; Attrs = @($l[1] -split ' '); Text = ($l[0] -replace "`0", '').TrimEnd() }
}
function Col($s, $needle) { $s.Row.IndexOf($needle) }
# Small tabs = a marker emoji (2 cells: char + NUL) directly followed by the number.
# Named tabs have a space between the marker and "N:name", so they never match.
# Returns @{ Nums = "2,3"; Cols = @{ '2' = col of the digit; ... } }.
function Small($s) {
  $m = [regex]::Matches($s.Row, "\x00(\d+) ")
  $cols = @{}
  foreach ($x in $m) { $cols[$x.Groups[1].Value] = $x.Groups[1].Index }
  @{ Nums = (@($m | ForEach-Object { $_.Groups[1].Value }) -join ','); Cols = $cols }
}
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function TraceSince($from, $pattern) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match $pattern } | ForEach-Object { [void]($_ -match $pattern); $Matches[0] }) }
# Poll the tab bar (up to $sec s) until $test (a script block taking the screen) is true.
function Wait-For($ccId, $sec, $test) {
  $s = $null
  $deadline = (Get-Date).AddSeconds($sec)
  do { $s = Send $ccId ''; if (& $test $s) { break }; Start-Sleep -Milliseconds 800 } while ((Get-Date) -lt $deadline)
  $s
}

$hwnd = [IntPtr]::Zero
if ($Wt) {
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
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr Find(string title) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => { if (!IsWindowVisible(h)) return true; var sb = new StringBuilder(512); GetWindowText(h, sb, 512); if (sb.ToString() == title) { found = h; return false; } return true; }, IntPtr.Zero);
    return found;
  }
}
"@
  Add-Type -AssemblyName System.Drawing
  [void][W]::SetProcessDPIAware()
  New-Item -ItemType Directory -Force $OutDir | Out-Null
}
# PrintWindow(PW_RENDERFULLCONTENT): works while other windows cover this one. Top 150 px.
function Shot($name) {
  if ([W]::IsIconic($hwnd)) { [void][W]::ShowWindow($hwnd, 4); Start-Sleep -Milliseconds 500 }
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($hwnd, [ref]$r)
  $full = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($full)
  $hdc = $g.GetHdc()
  [void][W]::PrintWindow($hwnd, $hdc, 2)
  $g.ReleaseHdc($hdc); $g.Dispose()
  $ch = [Math]::Min(150, $full.Height)
  $crop = $full.Clone((New-Object Drawing.Rectangle 0, 0, $full.Width, $ch), $full.PixelFormat)
  $p = Join-Path $OutDir $name
  $crop.Save($p, [Drawing.Imaging.ImageFormat]::Png)
  $crop.Dispose(); $full.Dispose()
  "   shot: $p ($((Get-Item $p).Length) bytes)"
}

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$p = $null; $cc = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
$prevFg = [IntPtr]::Zero
try {
  if ($Wt) {
    $prevFg = [W]::GetForegroundWindow()
    Start-Process wt.exe -ArgumentList @('-w', 'new', '--size', '100,14', '-d', "`"$work`"", 'cmd.exe', '/c', "`"$run`"")
  } else {
    # Hidden start; on-desktop2.ps1 moves the window to virtual desktop 2 before showing it.
    $p = Start-Process -FilePath "conhost.exe" -ArgumentList "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\on-desktop2.ps1`" `"$run`"" -WorkingDirectory $work -WindowStyle Hidden -PassThru
  }
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $cc = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1).Id
  } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw "no new ccnest.exe" }
  if ($Wt) {
    $deadline = (Get-Date).AddSeconds(15)
    do { Start-Sleep -Milliseconds 500; $hwnd = [W]::Find($tag) } while ($hwnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
    if ($hwnd -eq [IntPtr]::Zero) { throw "window '$tag' not found" }
    [void][W]::MoveWindow($hwnd, 0, 0, 1400, 420, $true)
    if ($prevFg -ne [IntPtr]::Zero) { [void][W]::SetForegroundWindow($prevFg) }
  }
  Start-Sleep -Seconds 5
  foreach ($i in 1..4) { [void](Send $cc 'ctrl:t'); Start-Sleep -Seconds 3 }

  if ($Off) {
    "Off. CCNEST_TAB_GROUP=off: wait 10 s, nothing is parked"
    Start-Sleep -Seconds 10
    $s = Send $cc ''
    "   tab row: " + $s.Text
    $cols = @('1:alpha', '2:bravo', '3:cc', '4:delta', '5:echo' | ForEach-Object { Col $s $_ })
    Check (@($cols | Where-Object { $_ -lt 0 }).Count -eq 0) "all five tabs show their names"
    Check ((($cols | Sort-Object) -join ',') -eq ($cols -join ',')) "in tab order"
    Check ((Col $s $DIV) -lt 0 -and (Small $s).Nums -eq '') "no divider, no small tab"
  } else {
    "A. after the grace: named tabs left, working tabs small on the right"
    $s = Wait-For $cc 20 { param($x) (Small $x).Nums -eq '2,3' }
    "   tab row: " + $s.Text
    $sm = Small $s
    Check ($sm.Nums -eq '2,3') "small tabs: 2, 3 (got '$($sm.Nums)')"
    Check ((Col $s '2:bravo') -lt 0 -and (Col $s '3:cc') -lt 0) "their names are not shown"
    $a = Col $s '1:alpha'; $d = Col $s '4:delta'; $v = Col $s $DIV; $e = Col $s '5:echo'
    Check ($a -ge 0 -and $a -lt $d -and $d -lt $v -and $v -lt $sm.Cols['2'] -and $sm.Cols['2'] -lt $sm.Cols['3'] -and $sm.Cols['3'] -lt $e) "order: alpha, delta | 2, 3, echo (cols $a, $d | $($sm.Cols['2']), $($sm.Cols['3']), $e)"
    Check ($e -ge 0) "the active tab (echo, working) keeps its name"
    $attrActive = $s.Attrs[$e + 2]
    if ($Wt) { Shot 'park-1-active-working.png' }

    "B. click alpha"
    $s = Send $cc "click:$($a + 3),0"
    "   tab row: " + $s.Text
    $sm = Small $s
    Check ($sm.Nums -eq '2,3,5' -and (Col $s '5:echo') -lt 0) "echo is small now: 2, 3, 5 (got '$($sm.Nums)')"
    Check ((Col $s '1:alpha') -eq $a -and (Col $s '4:delta') -eq $d) "alpha and delta did not move"
    Check ($s.Attrs[$a + 2] -eq $attrActive) "alpha is the active tab"
    if ($Wt) { Shot 'park-2-all-small.png' }

    if (-not $Wt) {
      "C. click small 3"
      $c3 = $sm.Cols['3']
      $s = Send $cc "click:$c3,0"
      "   tab row: " + $s.Text
      $sm = Small $s
      Check ((Col $s '3:cc') -eq $c3 + 1) "tab 3 shows its name in place (col $(Col $s '3:cc'), marker did not move)"
      Check ($sm.Nums -eq '2,5') "2 and 5 stay small (got '$($sm.Nums)')"
      Check ((Col $s '1:alpha') -eq $a -and (Col $s '4:delta') -eq $d) "the named group did not move"

      "D. double-click small 2"
      $n0 = TraceLen
      $s = Send $cc "dbl:$($sm.Cols['2']),0"
      "   tab row: " + $s.Text
      Check (@(TraceSince $n0 'begin_rename_tab idx=1').Count -eq 1) "the name input opened on tab 2 (begin_rename_tab idx=1)"
      $s = Send $cc 'esc'
      Check ((Col $s '2:bravo') -ge 0) "after Esc tab 2 is the active tab, named, still on the right"

      "E. cc stops working"
      New-Item -ItemType File (Join-Path $work 'idle3') | Out-Null
      $s = Wait-For $cc 15 { param($x) ((Col $x '3:cc') -ge 0) -and ((Col $x '3:cc') -lt (Col $x $DIV)) }
      "   tab row: " + $s.Text
      $a = Col $s '1:alpha'; $c = Col $s '3:cc'; $d = Col $s '4:delta'; $v = Col $s $DIV
      Check ($a -ge 0 -and $a -lt $c -and $c -lt $d -and $d -lt $v) "cc is back in the named group, in tab order: alpha, cc, delta | (cols $a, $c, $d | $v)"
      Check ((Small $s).Nums -eq '5' -and (Col $s '2:bravo') -gt $v) "right group: bravo (active, named) and small 5"

      "F. drag alpha onto cc, then far right over the parked group"
      $n0 = TraceLen
      $s = Send $cc "drag:$($a + 3),0,$($c + 3),0"
      "   tab row: " + $s.Text
      $m = TraceSince $n0 'move_tab from=\d+ to=\d+'
      Check (($m -join '/') -eq 'move_tab from=0 to=2') "one move past cc, skipping bravo in between (got '$($m -join '/')')"
      # Tab order is now bravo, cc, alpha, delta, echo = numbers 1..5.
      $c = Col $s '2:cc'; $a = Col $s '3:alpha'; $d = Col $s '4:delta'; $v = Col $s $DIV
      Check ($c -ge 0 -and $c -lt $a -and $a -lt $d -and $d -lt $v) "named group: cc, alpha, delta | (cols $c, $a, $d | $v)"
      $n0 = TraceLen
      $far = (Small $s).Cols['5'] + 1
      $s = Send $cc "drag:$($a + 3),0,$far,0"
      "   tab row: " + $s.Text
      $m = TraceSince $n0 'move_tab from=\d+ to=\d+'
      Check (($m -join '/') -eq 'move_tab from=2 to=3') "pulled over the parked group it stops at the end of the named group (got '$($m -join '/')')"
      $d = Col $s '3:delta'; $a = Col $s '4:alpha'; $v = Col $s $DIV
      Check ($d -ge 0 -and $d -lt $a -and $a -lt $v -and (Small $s).Nums -eq '1,5') "named group: cc, delta, alpha | small 1, 5 (the dragged tab is the active one)"

      "G. Alt+Right from alpha (last named tab)"
      $s = Send $cc 'altright'
      "   tab row: " + $s.Text
      Check ($s.Attrs[(Col $s '1:bravo') + 2] -eq $attrActive) "the first tab of the right group (bravo) is active"
      $s = Send $cc 'altright'
      "   tab row: " + $s.Text
      Check ((Col $s '5:echo') -ge 0 -and (Small $s).Nums -eq '1') "next: echo (shows its name), bravo is small"
      $s = Send $cc 'altright'
      "   tab row: " + $s.Text
      Check ($s.Attrs[(Col $s '2:cc') + 2] -eq $attrActive) "next wraps to the first named tab (cc)"
    }
  }

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc }
  if ($p) { Kill-Tree $p.Id }
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
