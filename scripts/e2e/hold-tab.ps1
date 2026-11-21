# Alt+C puts the front tab on hold: it goes to the far right of the tab bar as a grey dot +
# number and stays there until Alt+C is pressed again (v0.1.48) - check on the real binary.
# Starts a throw-away ccnest in conhost with a fake claude per tab:
#   1 alpha = idle (needs you)   2 bravo = working   3 cc = idle   4 delta = idle   5 echo = idle
# "working" = the pane shows "esc to interrupt" until the file idle<N> appears in the work
# folder; then the fake claude clears the screen (= needs you).
# Keys and mouse go in through WriteConsoleInputW (inner mode of dblclick-rename-tab.ps1),
# so this never collides with the user's keyboard and mouse. The user's ccnest is not touched.
#
#  A. start: alpha, cc, delta, echo named on the left | small green 2. Nothing is on hold.
#  B. Alt+C on echo (front) -> a second divider and the dot + 5 at the far right, the name is
#     gone, the front moves to the neighbour (delta), the session id is in the holds file
#  C. click the held tab -> it shows its name in place (still on hold); Alt+C again -> back in
#     the named group at its old place, the front stays, the holds file is empty
#  D. click alpha, Alt+C -> the front moves to the right neighbour of the same group (cc)
#  E. click the small green 2 (bravo), Alt+C -> held; bravo then stops working (needs you):
#     it stays a grey dot (no name, not back on the left)
#  F. middle-click the held tab 1 (close), Ctrl+Shift+T -> the reopened tab is still on hold
#  G. Ctrl+C does not hold a tab (it is the interrupt key)
#  H. a second window started with --resume: the session that is in the holds file comes up
#     on hold
# -Off: CCNEST_TAB_GROUP=off -> a held tab is drawn small in place, no divider.
# -Wt: same start in a Windows Terminal window, screenshots (PrintWindow) of B and E.
# ASCII only (no BOM needed).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [switch]$Off, [switch]$Wt,
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-hold-tab-shots')
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$tag = 'ccnest-e2e-hold-tab-' + (Get-Date -Format 'HHmmss')
$work = Join-Path $env:TEMP $tag
New-Item -ItemType Directory -Force $work | Out-Null
$counter = Join-Path $work 'n.txt'
$shim = Join-Path $work 'claude-shim.cmd'
$holds = Join-Path $work 'tab-holds.json'
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
function Write-Run($path, $tail) {
  $l = @(
    '@echo off', 'set CCNEST_PID=', 'set CCNEST_JP_TITLE=off', 'set CCNEST_USAGE=off', 'set CCNEST_INPUT_TRACE=1',
    "set CCNEST_TAB_NAMES=$(Join-Path $work 'tab-names.json')", "set CCNEST_TAB_HOLDS=$holds", "set CCNEST_CLAUDE_BIN=$shim"
  )
  if ($Off) { $l += 'set CCNEST_TAB_GROUP=off' }
  $l += "`"$exe`"$tail"
  Set-Content $path $l -Encoding ASCII
}
$run = Join-Path $work 'run.cmd'
Write-Run $run ''
$screenFile = Join-Path $work 'screen.txt'
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$DIV = [string][char]0x2502   # the divider between the groups
$DOT = [string][char]0x25CF   # the marker of a tab on hold

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Send($ccId, $ops) {
  if (-not $ops) { $ops = 'sleep:1' }
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"") -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 2) { throw "inner failed: $($l -join ' ')" }
  @{ Row = $l[0]; Attrs = @($l[1] -split ' '); Text = ($l[0] -replace "`0", '').TrimEnd() }
}
function Col($s, $needle) { $s.Row.IndexOf($needle) }
function Count($s, $needle) { ([regex]::Matches($s.Row, [regex]::Escape($needle))).Count }
# Held small tabs = the dot directly followed by the number (conhost may draw the dot two
# cells wide, then a NUL sits between them). A held tab that shows its name has a space
# after the dot, so it never matches. Returns @{ Nums = "1,5"; Cols = @{ '5' = col of the dot } }.
function Held($s) {
  $m = [regex]::Matches($s.Row, "$DOT\x00?(\d+) ")
  $cols = @{}
  foreach ($x in $m) { $cols[$x.Groups[1].Value] = $x.Index }
  @{ Nums = (@($m | ForEach-Object { $_.Groups[1].Value }) -join ','); Cols = $cols }
}
# Small green tabs = a marker emoji (2 cells: char + NUL) directly followed by the number.
function Green($s) {
  $m = [regex]::Matches($s.Row, "[^$DOT]\x00(\d+) ")
  (@($m | ForEach-Object { $_.Groups[1].Value }) -join ',')
}
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function TraceSince($from, $pattern) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match $pattern } | ForEach-Object { [void]($_ -match $pattern); $Matches[0] }) }
function HoldCount { if (Test-Path $holds) { @((Get-Content $holds -Raw -Encoding UTF8 | ConvertFrom-Json).PSObject.Properties).Count } else { 0 } }
# Poll the tab bar (up to $sec s) until $test (a script block taking the screen) is true.
function Wait-For($ccId, $sec, $test) {
  $s = $null
  $deadline = (Get-Date).AddSeconds($sec)
  do { $s = Send $ccId ''; if (& $test $s) { break }; Start-Sleep -Milliseconds 800 } while ((Get-Date) -lt $deadline)
  $s
}
function New-Ccnest($runFile, $before) {
  $proc = $null
  if ($Wt) {
    Start-Process wt.exe -ArgumentList @('-w', 'new', '--size', '100,14', '-d', "`"$work`"", 'cmd.exe', '/c', "`"$runFile`"")
  } else {
    # Hidden start; on-desktop2.ps1 moves the window to virtual desktop 2 before showing it.
    $proc = Start-Process -FilePath "conhost.exe" -ArgumentList "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\on-desktop2.ps1`" `"$runFile`"" -WorkingDirectory $work -WindowStyle Hidden -PassThru
  }
  $id = $null
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $id = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Select-Object -First 1).Id
  } while (-not $id -and (Get-Date) -lt $deadline)
  if (-not $id) { throw "no new ccnest.exe" }
  @{ Proc = $proc; Id = $id }
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
$p = $null; $cc = $null; $p2 = $null; $cc2 = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
$prevFg = [IntPtr]::Zero
try {
  if ($Wt) { $prevFg = [W]::GetForegroundWindow() }
  $started = New-Ccnest $run $beforePids
  $p = $started.Proc; $cc = $started.Id
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
    "Off. CCNEST_TAB_GROUP=off: Alt+C on echo -> small in place, no divider"
    Start-Sleep -Seconds 6
    $s = Send $cc ''
    $d = Col $s '4:delta'
    $attrActive = $s.Attrs[(Col $s '5:echo') + 2]
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    $h = Held $s
    Check ((TraceSince $n0 'hold_tab idx=4 held=true active=3').Count -eq 1) "hold_tab idx=4 held=true active=3"
    Check ($h.Nums -eq '5' -and (Col $s '5:echo') -lt 0) "echo is a dot + 5 (got '$($h.Nums)')"
    Check ((Col $s $DIV) -lt 0) "no divider"
    $cols = @('1:alpha', '2:bravo', '3:cc', '4:delta' | ForEach-Object { Col $s $_ })
    Check (@($cols | Where-Object { $_ -lt 0 }).Count -eq 0 -and (($cols | Sort-Object) -join ',') -eq ($cols -join ',') -and $h.Cols['5'] -gt $cols[3]) "the other four keep their names, in tab order, the dot is after delta"
    Check ($s.Attrs[$d + 2] -eq $attrActive) "delta is the front tab"
  } else {
    "A. start: named tabs left, the working tab small on the right, nothing on hold"
    $s = Wait-For $cc 20 { param($x) (Green $x) -eq '2' }
    "   tab row: " + $s.Text
    $a = Col $s '1:alpha'; $c = Col $s '3:cc'; $d = Col $s '4:delta'; $e = Col $s '5:echo'; $v = Col $s $DIV
    Check ((Green $s) -eq '2' -and (Held $s).Nums -eq '') "small green: 2, on hold: none"
    Check ($a -ge 0 -and $a -lt $c -and $c -lt $d -and $d -lt $e -and $e -lt $v -and (Count $s $DIV) -eq 1) "order: alpha, cc, delta, echo | 2 (one divider)"
    $attrActive = $s.Attrs[$e + 2]
    $attrNamed = $s.Attrs[$a + 2]

    "B. Alt+C on echo (front)"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    $h = Held $s
    Check ((TraceSince $n0 'hold_tab idx=4 held=true active=3').Count -eq 1) "hold_tab idx=4 held=true active=3"
    Check ($h.Nums -eq '5' -and (Col $s '5:echo') -lt 0) "echo is a dot + 5, its name is gone (got '$($h.Nums)')"
    Check ((Count $s $DIV) -eq 2 -and $h.Cols['5'] -gt $s.Row.LastIndexOf($DIV)) "two dividers, the held tab is right of the last one"
    Check ((Green $s) -eq '2' -and (Col $s '1:alpha') -eq $a -and (Col $s '4:delta') -eq $d) "the green tab and the named tabs did not move"
    Check ($s.Attrs[$d + 2] -eq $attrActive) "the front moved to delta"
    $attrHeld = $s.Attrs[$h.Cols['5']]
    Check ($attrHeld -ne $attrNamed -and $attrHeld -ne $attrActive) "the held tab has its own colour (attr $attrHeld, named $attrNamed, front $attrActive)"
    Check ((HoldCount) -eq 1) "the holds file has 1 session (got $(HoldCount))"
    if ($Wt) { Shot 'hold-1-one-held.png' }

    "C. click the held tab, then Alt+C again"
    $s = Send $cc "click:$($h.Cols['5']),0"
    "   tab row: " + $s.Text
    $e2 = Col $s '5:echo'
    Check ($e2 -gt $s.Row.LastIndexOf($DIV) -and (Held $s).Nums -eq '') "it shows its name in place, right of the last divider"
    Check ($s.Row.Substring(0, $e2).TrimEnd(" `0".ToCharArray()).EndsWith($DOT)) "the name still carries the dot (on hold)"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    Check ((TraceSince $n0 'hold_tab idx=4 held=false active=4').Count -eq 1) "hold_tab idx=4 held=false active=4 (the front stays)"
    Check ((Col $s '5:echo') -eq $e -and (Count $s $DIV) -eq 1 -and (Count $s $DOT) -eq 0) "echo is back at its old place in the named group, one divider, no dot"
    Check ((HoldCount) -eq 0) "the holds file is empty (got $(HoldCount))"

    "D. click alpha, Alt+C"
    [void](Send $cc "click:$($a + 3),0")
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    Check ((TraceSince $n0 'hold_tab idx=0 held=true active=2').Count -eq 1) "hold_tab idx=0 held=true active=2 (cc: right neighbour in the named group, not the green bravo)"
    Check ((Held $s).Nums -eq '1' -and (Col $s '1:alpha') -lt 0) "alpha is a dot + 1"
    Check ($s.Attrs[(Col $s '3:cc') + 2] -eq $attrActive) "cc is the front tab"

    if (-not $Wt) {
      "E. hold the working tab (bravo), then it stops working"
      $g2 = [regex]::Match($s.Row, "[^$DOT]\x00(2) ").Groups[1].Index
      [void](Send $cc "click:$g2,0")
      $n0 = TraceLen
      $s = Send $cc 'alt:c'
      "   tab row: " + $s.Text
      Check ((TraceSince $n0 'hold_tab idx=1 held=true active=4').Count -eq 1) "hold_tab idx=1 held=true active=4 (echo: the nearest tab that is not on hold)"
      Check ((Held $s).Nums -eq '1,2' -and (Green $s) -eq '') "on hold: 1, 2 - no green tab left (got '$((Held $s).Nums)' / '$(Green $s)')"
      New-Item -ItemType File (Join-Path $work 'idle2') | Out-Null
      Start-Sleep -Seconds 8
      $s = Send $cc ''
      "   tab row: " + $s.Text
      Check ((Held $s).Nums -eq '1,2' -and (Col $s '2:bravo') -lt 0) "bravo needs you now, but stays a dot + 2 (no name, not back on the left)"
      Check ((HoldCount) -eq 2) "the holds file has 2 sessions (got $(HoldCount))"

      "F. middle-click the held tab 1 (close), Ctrl+Shift+T"
      $s = Send $cc "mclick:$((Held $s).Cols['1']),0"
      "   tab row: " + $s.Text
      # Numbers shift down: bravo is tab 1 now (still on hold), cc 2, delta 3, echo 4.
      Check ((Held $s).Nums -eq '1' -and (Col $s '4:echo') -ge 0) "closed: one held tab left (bravo, now number 1)"
      $s = Send $cc 'ctrlshift:t|sleep:2500'
      "   tab row: " + $s.Text
      # The reopened tab is the front tab (it shows a name) and still carries the dot.
      $o = Col $s '1:other6'
      Check ($o -gt $s.Row.LastIndexOf($DIV) -and $s.Row.Substring(0, $o).TrimEnd(" `0".ToCharArray()).EndsWith($DOT)) "the reopened tab is still on hold (dot + 1:other6, right of the last divider)"
      Check ((Held $s).Nums -eq '2') "bravo is number 2 again, on hold"

      "G. Ctrl+C is not the hold key"
      [void](Send $cc "click:$((Col $s '5:echo') + 3),0")
      $n0 = TraceLen
      $s = Send $cc 'ctrl:c|sleep:600'
      "   tab row: " + $s.Text
      Check ((TraceSince $n0 "Key\(Char\('c'\),Press,KeyModifiers\(CONTROL\)\)").Count -eq 1) "Ctrl+C reached ccnest"
      # The reopened tab is not the front tab any more, so it is small too: 1 and 2.
      Check ((TraceSince $n0 'hold_tab').Count -eq 0 -and (Col $s '5:echo') -ge 0 -and (Held $s).Nums -eq '1,2') "no hold_tab, echo keeps its name (on hold: '$((Held $s).Nums)')"

      "H. a second window with --resume: the held session comes up on hold"
      $u1 = [guid]::NewGuid().ToString(); $u2 = [guid]::NewGuid().ToString()
      $json = Get-Content $holds -Raw -Encoding UTF8 | ConvertFrom-Json
      $json | Add-Member -NotePropertyName $u2 -NotePropertyValue 1
      [IO.File]::WriteAllText($holds, ($json | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding $false))
      $run2 = Join-Path $work 'run2.cmd'
      Write-Run $run2 " `"$work`" --resume $u1=first --resume $u2=second"
      $started2 = New-Ccnest $run2 (@($beforePids) + $cc)
      $p2 = $started2.Proc; $cc2 = $started2.Id
      $s = Wait-For $cc2 20 { param($x) (Held $x).Nums -eq '2' }
      "   tab row: " + $s.Text
      Check ((Held $s).Nums -eq '2' -and (Count $s $DIV) -eq 1) "tab 2 (the session in the holds file) is a dot + 2 behind a divider"
      Check ((Count $s $DOT) -eq 1) "tab 1 is not on hold"
    } else {
      [void](Send $cc "click:$((Col $s '4:delta') + 3),0")
      $s = Send $cc 'alt:c'
      "   tab row: " + $s.Text
      Shot 'hold-2-two-held.png'
    }
  }

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc2) { Kill-Tree $cc2 }
  if ($p2) { Kill-Tree $p2.Id }
  if ($cc) { Kill-Tree $cc }
  if ($p) { Kill-Tree $p.Id }
  if ($hwnd -ne [IntPtr]::Zero) { [void][W]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
