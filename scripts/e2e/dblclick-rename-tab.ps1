# Double-click a tab to rename it (v0.1.45) - check on the real binary.
# Starts a throw-away ccnest in conhost (fake claude = a cmd that sets a title per tab and
# waits) and writes keys / mouse straight into that console with WriteConsoleInputW (no
# SendInput = never collides with the user's keyboard and mouse). The user's own ccnest
# windows are not touched, and CCNEST_TAB_NAMES points at a temp file so the real
# %APPDATA%\ccnest\tab-names.json is not touched either.
#
# Three tabs (alpha / bravo-long-tab-name / cc):
#  A. double-click bravo -> the tab becomes an input holding the current name
#  B. type "pay" + Enter -> tab 2 reads "2:pay"; it still does after two 2 s ticks (the
#     automatic name does not come back); tab-names.json holds 1 entry "pay"
#  C. double-click bravo, type two Japanese chars as ONE burst (how an IME commit arrives),
#     Enter -> "2:<those chars>"
#  D. double-click bravo, Enter without typing -> name unchanged, no rename_tab trace line
#  E. slow double-click (700 ms apart) on alpha -> no input opens (just a click)
#  F. double-click alpha, type "zz", click inside the pane -> committed "1:zz"
#  G. double-click alpha, drag the tab to the right end while the input is open -> no
#     move_tab, order unchanged; then click on tab cc -> committed nothing (unchanged) and
#     alpha stays the active tab (the click is swallowed)
#  H. double-click alpha, chars + Enter in ONE burst (= seen as a paste) -> goes into the
#     input, the input stays open; Enter -> "1:ab"
#  I. double-click alpha, Backspace, Enter -> the hand-made name is removed (automatic name
#     "1:alpha" is back) and its tab-names.json entry is gone
#  J. middle-click bravo (close), Ctrl+Shift+T (reopen) -> the reopened tab keeps its name
#  K. quit, start a new ccnest with --resume <session id of bravo> -> "1:<name>" again
# The Japanese text is written as code points so this file stays ASCII.
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Inner, [int]$ProcId, [string]$Out, [string]$Ops,
  # Inner mode: more screen rows to append after the two standard lines, e.g. "1,2,-1"
  # (-1 = the last row). Each is written as its text, one line per row.
  [string]$AlsoRows
)
$ErrorActionPreference = 'Stop'

if ($Inner) {
  trap { ("inner error: " + $_ + "`n" + $_.InvocationInfo.PositionMessage) | Set-Content $Out -Encoding UTF8; exit 1 }
  Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class K {
  [DllImport("kernel32.dll")] public static extern bool FreeConsole();
  [DllImport("kernel32.dll")] public static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr CreateFileW(string n, uint a, uint s, IntPtr sa, uint c, uint f, IntPtr t);
  [StructLayout(LayoutKind.Sequential)] public struct COORD { public short X, Y; }
  [StructLayout(LayoutKind.Sequential)] public struct SMALL_RECT { public short L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct INFO { public COORD Size; public COORD Cur; public short Attr; public SMALL_RECT Win; public COORD Max; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct CHAR_INFO { public char Ch; public short Attr; }
  [DllImport("kernel32.dll")] public static extern bool GetConsoleScreenBufferInfo(IntPtr h, out INFO i);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern bool ReadConsoleOutputW(IntPtr h, [Out] CHAR_INFO[] buf, COORD size, COORD at, ref SMALL_RECT r);
  [StructLayout(LayoutKind.Explicit, CharSet=CharSet.Unicode)] public struct KEY_REC {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public int KeyDown;
    [FieldOffset(8)] public short Repeat;
    [FieldOffset(10)] public short Vk;
    [FieldOffset(12)] public short Scan;
    [FieldOffset(14)] public char Ch;
    [FieldOffset(16)] public uint Ctrl;
  }
  [StructLayout(LayoutKind.Explicit)] public struct MOUSE_REC {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public COORD Pos;
    [FieldOffset(8)] public uint Buttons;
    [FieldOffset(12)] public uint Ctrl;
    [FieldOffset(16)] public uint Flags;
  }
  [DllImport("kernel32.dll", EntryPoint="WriteConsoleInputW")] public static extern bool WriteKeys(IntPtr h, KEY_REC[] r, uint n, out uint w);
  [DllImport("kernel32.dll", EntryPoint="WriteConsoleInputW")] public static extern bool WriteMouse(IntPtr h, MOUSE_REC[] r, uint n, out uint w);
  static IntPtr outH, inH; static INFO info;
  public static bool Attach(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    outH = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    inH = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    return GetConsoleScreenBufferInfo(outH, out info);
  }
  // Screen row r, one char per cell (the 2nd cell of a wide char is '\0').
  public static int Height() { return info.Win.B - info.Win.T + 1; }
  public static string Row(int r, out short[] attrs) {
    short w = info.Size.X;
    var buf = new CHAR_INFO[w];
    var rect = new SMALL_RECT { L = 0, T = (short)(info.Win.T + r), R = (short)(w - 1), B = (short)(info.Win.T + r) };
    ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
    var sb = new StringBuilder();
    attrs = new short[w];
    for (int i = 0; i < w; i++) { sb.Append((buf[i].Attr & 0x200) != 0 ? '\0' : buf[i].Ch); attrs[i] = buf[i].Attr; }
    return sb.ToString();
  }
  static void Pair(List<KEY_REC> l, short vk, char ch, uint ctrl) {
    l.Add(new KEY_REC { EventType = 1, KeyDown = 1, Repeat = 1, Vk = vk, Ch = ch, Ctrl = ctrl });
    l.Add(new KEY_REC { EventType = 1, KeyDown = 0, Repeat = 1, Vk = vk, Ch = ch, Ctrl = ctrl });
  }
  // One key (press + release). vk 0 = a plain character (how IME text arrives).
  public static void Key(short vk, char ch, uint ctrl) {
    var l = new List<KEY_REC>(); Pair(l, vk, ch, ctrl);
    uint n; WriteKeys(inH, l.ToArray(), (uint)l.Count, out n);
  }
  // Several characters in ONE write (they reach ccnest in one batch). enter = add Enter.
  public static void Burst(string text, bool enter) {
    var l = new List<KEY_REC>();
    foreach (char c in text) Pair(l, 0, c, 0);
    if (enter) Pair(l, 0x0D, '\r', 0);
    uint n; WriteKeys(inH, l.ToArray(), (uint)l.Count, out n);
  }
  // buttons: 1 = left held, 4 = middle held. flags: 0 = press / release, 1 = moved.
  public static void Mouse(int col, int row, uint buttons, uint flags) {
    var p = new COORD { X = (short)col, Y = (short)(info.Win.T + row) };
    var recs = new MOUSE_REC[] { new MOUSE_REC { EventType = 2, Pos = p, Buttons = buttons, Ctrl = 0, Flags = flags } };
    uint n; WriteMouse(inH, recs, 1, out n);
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  function Uni($hex) { -join ($hex -split ',' | ForEach-Object { [char][Convert]::ToInt32($_, 16) }) }
  function Click($c, $r) { [K]::Mouse($c, $r, 1, 0); Start-Sleep -Milliseconds 40; [K]::Mouse($c, $r, 0, 0) }
  # Ops = "op:arg|op:arg|...".
  foreach ($op in @($Ops -split '\|' | Where-Object { $_ })) {
    $name, $arg = $op -split ':', 2
    $n = @()
    if ($arg -and $name -notin 'type', 'typeu', 'burstu', 'pasteu', 'ctrl', 'ctrlshift', 'alt') { $n = @($arg -split ',' | ForEach-Object { [int]$_ }) }
    switch ($name) {
      'click' { Click $n[0] $n[1] }
      'dbl' { Click $n[0] $n[1]; Start-Sleep -Milliseconds 120; Click $n[0] $n[1] }
      'slowdbl' { Click $n[0] $n[1]; Start-Sleep -Milliseconds 700; Click $n[0] $n[1] }
      'mclick' { [K]::Mouse($n[0], $n[1], 4, 0); Start-Sleep -Milliseconds 40; [K]::Mouse($n[0], $n[1], 0, 0) }
      # drag:c0,r0,c1,r1 = press at c0, pull one column at a time to c1, release.
      'drag' {
        [K]::Mouse($n[0], $n[1], 1, 0); Start-Sleep -Milliseconds 120
        foreach ($c in ($n[0] + 1)..$n[2]) { [K]::Mouse($c, $n[3], 1, 1); Start-Sleep -Milliseconds 40 }
        [K]::Mouse($n[2], $n[3], 0, 0)
      }
      'type' { foreach ($ch in $arg.ToCharArray()) { [K]::Key([int16][char]::ToUpper($ch), $ch, 0); Start-Sleep -Milliseconds 80 } }
      'typeu' { foreach ($ch in (Uni $arg).ToCharArray()) { [K]::Key(0, $ch, 0); Start-Sleep -Milliseconds 80 } }
      'burstu' { [K]::Burst((Uni $arg), $false) }
      'pasteu' { [K]::Burst((Uni $arg), $true) }
      'enter' { [K]::Key(0x0D, [char]13, 0) }
      'esc' { [K]::Key(0x1B, [char]27, 0) }
      'bs' { [K]::Key(0x08, [char]8, 0) }
      # Ctrl+<letter>: uChar is the control char (Ctrl+T = 0x14). 0x8 = LEFT_CTRL, 0x10 = SHIFT.
      'ctrl' { $u = [char]::ToUpper($arg[0]); [K]::Key([int16]$u, [char]([int]$u - 64), 0x8) }
      'ctrlshift' { $u = [char]::ToUpper($arg[0]); [K]::Key([int16]$u, [char]([int]$u - 64), 0x18) }
      # Alt+<letter> (Alt+C = hold): uChar is the plain letter, 0x2 = LEFT_ALT.
      'alt' { $u = [char]::ToUpper($arg[0]); [K]::Key([int16]$u, [char]::ToLower($arg[0]), 0x2) }
      # Alt+Right / Alt+Left (next / previous tab). 0x2 = LEFT_ALT, 0x100 = ENHANCED_KEY.
      'altright' { [K]::Key(0x27, [char]0, 0x102) }
      'altleft' { [K]::Key(0x25, [char]0, 0x102) }
      'sleep' { Start-Sleep -Milliseconds $n[0] }
      # wheelup:c,r,count = turn the wheel up (back into the history) count notches at (c, r).
      # Buttons high word = +120 per notch (up), flags 4 = MOUSE_WHEELED.
      'wheelup' { foreach ($i in 1..$n[2]) { [K]::Mouse($n[0], $n[1], [uint32]0x00780000, 4); Start-Sleep -Milliseconds 60 } }
    }
    Start-Sleep -Milliseconds 150
  }
  # Let the tab slide (140 ms) and the redraw finish before reading.
  Start-Sleep -Milliseconds 400
  $attrs = $null
  $row = [K]::Row(0, [ref]$attrs)
  $lines = @($row, (($attrs | ForEach-Object { '{0:x}' -f ($_ -band 0xff) }) -join ' '))
  # The extra rows are read before the console is released.
  foreach ($r in @($AlsoRows -split '[,; ]' | Where-Object { $_ })) {
    $i = [int]$r; if ($i -lt 0) { $i = [K]::Height() + $i }
    $a2 = $null
    $lines += ([K]::Row($i, [ref]$a2) -replace "`0", '').TrimEnd()
  }
  [void][K]::FreeConsole()
  [IO.File]::WriteAllLines($Out, $lines, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$tag = 'ccnest-e2e-dblclick-rename-' + (Get-Date -Format 'HHmmss')
$work = Join-Path $env:TEMP $tag
New-Item -ItemType Directory -Force $work | Out-Null
$counter = Join-Path $work 'n.txt'
$shim = Join-Path $work 'claude-shim.cmd'
$store = Join-Path $work 'tab-names.json'
# Fake claude: counts its launches and sets a different title per tab, then waits.
# ccnest shows the pane title as the tab's task name (the "automatic" name).
Set-Content $shim @(
  '@echo off',
  'setlocal',
  'set N=0',
  "if exist `"$counter`" set /p N=<`"$counter`"",
  'set /a N=N+1',
  ">`"$counter`" echo %N%",
  'set T=other%N%',
  'if %N%==1 set T=alpha',
  'if %N%==2 set T=bravo-long-tab-name',
  'if %N%==3 set T=cc',
  'title %T%',
  'echo SHIM %N%',
  'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600'
) -Encoding ASCII
function Write-Run($file, $extraArgs) {
  Set-Content $file @(
    '@echo off',
    'set CCNEST_PID=',
    'set CCNEST_JP_TITLE=off',
    'set CCNEST_USAGE=off',
    'set CCNEST_INPUT_TRACE=1',
    "set CCNEST_TAB_NAMES=$store",
    "set CCNEST_CLAUDE_BIN=$shim",
    "`"$exe`" $extraArgs"
  ) -Encoding ASCII
}
$run = Join-Path $work 'run.cmd'
Write-Run $run ''
$screenFile = Join-Path $work 'screen.txt'
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$CUR = [string][char]0x258E          # the input cursor drawn after the name
$JP = -join ([char]0x6C7A, [char]0x6E08)   # two Japanese chars (U+6C7A U+6E08)

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Send($ccId, $ops) {
  if (-not $ops) { $ops = 'sleep:1' }
  $a = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"")
  Start-Process powershell.exe -ArgumentList $a -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 2) { throw "inner failed: $($l -join ' ')" }
  @{ Row = $l[0]; Attrs = @($l[1] -split ' '); Text = ($l[0] -replace "`0", '').TrimEnd() }
}
function Col($s, $needle) { $s.Row.IndexOf($needle) }
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function TraceSince($from, $pattern) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match $pattern }) }
function Store { if (Test-Path $store) { Get-Content $store -Raw -Encoding UTF8 | ConvertFrom-Json } else { $null } }
function StoreKeys { $o = Store; if ($o) { @($o.PSObject.Properties | ForEach-Object { $_.Name }) } else { @() } }
function Start-Cc($runFile) {
  $before = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
  # No console window at all (CREATE_NO_WINDOW): nothing shows up and nothing takes the keyboard
  # focus from the user. (A hidden conhost window still becomes the foreground window.) The
  # screen is read and input is sent through the console API, which needs no window.
  $si = New-Object Diagnostics.ProcessStartInfo 'cmd.exe', "/c `"$runFile`""
  $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.WorkingDirectory = $work
  $proc = [Diagnostics.Process]::Start($si)
  $deadline = (Get-Date).AddSeconds(20); $id = $null
  do {
    Start-Sleep -Milliseconds 500
    $id = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Select-Object -First 1).Id
  } while (-not $id -and (Get-Date) -lt $deadline)
  if (-not $id) { throw "no new ccnest.exe" }
  @{ Host = $proc; Cc = $id }
}
# Poll the tab bar (up to $sec s) until it contains $needle.
function Wait-Row($ccId, $needle, $sec) {
  $s = $null
  for ($i = 0; $i -lt $sec; $i++) { $s = Send $ccId ''; if ((Col $s $needle) -ge 0) { break }; Start-Sleep -Milliseconds 700 }
  $s
}

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$h = $null; $h2 = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  $h = Start-Cc $run
  $cc = $h.Cc
  Start-Sleep -Seconds 5
  [void](Send $cc 'ctrl:t'); Start-Sleep -Seconds 3
  [void](Send $cc 'ctrl:t'); Start-Sleep -Seconds 3
  $s = Wait-Row $cc '3:cc' 12
  "tab row: " + $s.Text
  if ((Col $s '1:alpha') -lt 0 -or (Col $s '2:bravo') -lt 0 -or (Col $s '3:cc') -lt 0) { throw "tab names did not appear" }
  $alpha = (Col $s '1:alpha') + 3
  $bravo = (Col $s '2:bravo') + 4
  $ccCol = (Col $s '3:cc') + 2
  $attrActive = $s.Attrs[$ccCol]     # tab 3 is the active tab right after Ctrl+T

  "A. double-click bravo"
  $n0 = TraceLen
  $s = Send $cc "dbl:$bravo,0"
  "   tab row: " + $s.Text
  Check ((Col $s ("bravo-long-tab-name" + $CUR)) -ge 0) "the tab is an input holding the full current name + cursor"
  Check (@(TraceSince $n0 'begin_rename_tab idx=1').Count -eq 1) "trace: begin_rename_tab idx=1 (once)"
  $attrSel = $s.Attrs[(Col $s 'bravo-long-tab-name') + 2]
  Check ($attrSel -ne $attrActive) "the untouched name is drawn in its own colour (attr $attrSel, active tab $attrActive)"

  "B. type 'pay' + Enter"
  $s = Send $cc 'type:pay'
  "   tab row: " + $s.Text
  Check ((Col $s ("pay" + $CUR)) -ge 0 -and (Col $s 'bravo') -lt 0) "the first key replaced the whole name"
  $n0 = TraceLen
  $s = Send $cc 'enter'
  "   tab row: " + $s.Text
  Check ((Col $s '2:pay') -ge 0 -and (Col $s $CUR) -lt 0) "committed: tab 2 reads 2:pay"
  Check (@(TraceSince $n0 'rename_tab idx=1 name=Some\("pay"\)').Count -eq 1) "trace: rename_tab idx=1 name=Some(`"pay`")"
  Start-Sleep -Seconds 5
  $s = Send $cc ''
  Check ((Col $s '2:pay') -ge 0) "still 2:pay after two 2 s ticks (the automatic name did not come back)"
  $keys = @(StoreKeys)
  Check ($keys.Count -eq 1 -and (Store).($keys[0]).name -eq 'pay') "tab-names.json: 1 entry, name = pay"
  $bravoSession = $keys[0]

  "C. double-click bravo, two Japanese chars in one burst, Enter"
  $s = Send $cc "dbl:$bravo,0|burstu:6c7a,6e08"
  "   tab row: " + $s.Text
  Check ((Col $s ($JP[0] + "`0" + $JP[1] + "`0" + $CUR)) -ge 0) "both chars landed in the input"
  $s = Send $cc 'enter'
  "   tab row: " + $s.Text
  Check ((Col $s ("2:" + $JP[0])) -ge 0) "committed: tab 2 reads 2:<the two chars>"
  Check ((Store).$bravoSession.name -eq $JP) "tab-names.json: the same session now holds the Japanese name"

  "D. double-click bravo, Enter without typing"
  $n0 = TraceLen
  $s = Send $cc "dbl:$bravo,0|enter"
  "   tab row: " + $s.Text
  Check ((Col $s ("2:" + $JP[0])) -ge 0 -and (Col $s $CUR) -lt 0) "name unchanged, input closed"
  Check (@(TraceSince $n0 '[^_]rename_tab idx=').Count -eq 0) "no rename_tab trace line"

  "E. slow double-click (700 ms apart) on alpha"
  $n0 = TraceLen
  $s = Send $cc "slowdbl:$alpha,0"
  "   tab row: " + $s.Text
  Check ((Col $s $CUR) -lt 0 -and @(TraceSince $n0 'begin_rename_tab').Count -eq 0) "no input opens"
  Check ($s.Attrs[$alpha] -eq $attrActive) "alpha is the active tab (plain click)"

  "F. double-click alpha, type 'zz', click inside the pane"
  $s = Send $cc "dbl:$alpha,0|type:zz|click:60,6"
  "   tab row: " + $s.Text
  Check ((Col $s '1:zz') -ge 0 -and (Col $s $CUR) -lt 0) "the click outside committed: 1:zz"

  "G. input open on alpha: drag the tab right, then click tab cc"
  $n0 = TraceLen
  $s = Send $cc "dbl:$alpha,0"
  $c0 = (Col $s ("zz" + $CUR))
  $ccNow = (Col $s '3:cc') + 2
  $s = Send $cc "drag:$c0,0,$ccNow,0"
  "   tab row: " + $s.Text
  Check (@(TraceSince $n0 'move_tab').Count -eq 0) "no move_tab while the input is open"
  Check ((Col $s ("zz" + $CUR)) -ge 0 -and (Col $s ("zz" + $CUR)) -lt (Col $s '3:cc')) "the input is still open, order unchanged"
  $n1 = TraceLen
  $s = Send $cc "click:$ccNow,0"
  "   tab row: " + $s.Text
  Check ((Col $s '1:zz') -ge 0 -and (Col $s $CUR) -lt 0) "click on another tab closed the input, name unchanged"
  Check (@(TraceSince $n1 '[^_]rename_tab idx=').Count -eq 0) "nothing was typed, so nothing was renamed"
  Check ($s.Attrs[(Col $s '1:zz') + 2] -eq $attrActive) "alpha is still the active tab (the click was swallowed)"

  "H. double-click alpha, 'ab' + Enter in one burst (seen as a paste)"
  $s = Send $cc "dbl:$alpha,0|pasteu:61,62"
  "   tab row: " + $s.Text
  Check ((Col $s ("ab" + $CUR)) -ge 0) "the pasted text is in the input and the input is still open"
  $s = Send $cc 'enter'
  "   tab row: " + $s.Text
  Check ((Col $s '1:ab') -ge 0) "committed: 1:ab"

  "I. double-click alpha, Backspace, Enter"
  $n0 = TraceLen
  $s = Send $cc "dbl:$alpha,0|bs|enter"
  "   tab row: " + $s.Text
  Check ((Col $s '1:alpha') -ge 0) "the hand-made name is gone, the automatic name 1:alpha is back"
  Check (@(TraceSince $n0 'rename_tab idx=0 name=None').Count -eq 1) "trace: rename_tab idx=0 name=None"
  $keys = @(StoreKeys)
  Check ($keys.Count -eq 1 -and $keys[0] -eq $bravoSession) "tab-names.json: only bravo's entry is left"

  "J. middle-click bravo (close), Ctrl+Shift+T (reopen)"
  $b = (Col $s ("2:" + $JP[0])) + 1
  $s = Send $cc "mclick:$b,0"
  "   tab row: " + $s.Text
  Check ((Col $s $JP[0]) -lt 0) "bravo is closed"
  [void](Send $cc 'ctrlshift:t')
  $s = Wait-Row $cc ("2:" + $JP[0]) 10
  "   tab row: " + $s.Text
  Check ((Col $s ("2:" + $JP[0])) -ge 0) "the reopened tab keeps its hand-made name"

  "K. quit, start again with --resume <bravo's session>"
  Kill-Tree $cc; Kill-Tree $h.Host.Id; $h = $null
  Start-Sleep -Seconds 1
  $run2 = Join-Path $work 'run2.cmd'
  Write-Run $run2 "`"$work`" --resume $bravoSession"
  $h2 = Start-Cc $run2
  $s = Wait-Row $h2.Cc ("1:" + $JP[0]) 15
  "   tab row: " + $s.Text
  Check ((Col $s ("1:" + $JP[0])) -ge 0) "the resumed session shows the saved name"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  foreach ($x in @($h, $h2)) { if ($x) { Kill-Tree $x.Cc; Kill-Tree $x.Host.Id } }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
