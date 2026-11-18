# E2E for "extend the selection to the click" (v0.1.44): Alt / Shift + left click.
#
# Starts a throwaway ccnest in conhost (fake claude = click-extend-selection-shim.ps1,
# which prints the lines LN00 .. LN13) and writes mouse records straight into that
# console with WriteConsoleInputW. No SendInput, so it does not fight the user's own
# keyboard / mouse, and the user's ccnest windows are never touched.
#
# Steps (each one reads the highlighted cells back from the console):
#   1. plain click on LN02              -> nothing stays selected
#   2. Alt+click on LN08                -> LN02 .. LN08 selected (from the plain click)
#   3. Shift+click on LN10              -> the start stays, the end moves to LN10
#   4. Alt+click on LN05 (inside)       -> shrinks to LN02 .. LN05
#   5. Alt+click on LN00 (before it)    -> grows to LN00 .. LN05 (nothing dropped)
#   6. plain click                      -> selection cleared
# plus: CCNEST_INPUT_TRACE has exactly 4 `extend_selection` lines.
#
# -MouseMode runs the same steps against a pane whose app turned mouse reporting on
# (what Claude Code does in fullscreen). Step 0 proves ccnest saw the mode: a held
# plain press does not highlight the cell (it is held back as a possible click).
#
# "Highlighted" = the cell attribute differs from the one read before any selection.
# ASCII only (no BOM needed).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [switch]$MouseMode,
  [switch]$Inner, [int]$ProcId, [string]$Out
)
$ErrorActionPreference = 'Stop'

if ($Inner) {
  trap { ("inner error: " + $_ + "`n" + $_.InvocationInfo.PositionMessage) | Set-Content $Out -Encoding UTF8; exit 1 }
  Add-Type @"
using System;
using System.Text;
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
  [StructLayout(LayoutKind.Explicit)] public struct MOUSE_REC {
    [FieldOffset(0)] public short EventType;
    [FieldOffset(4)] public COORD Pos;
    [FieldOffset(8)] public uint Buttons;
    [FieldOffset(12)] public uint Ctrl;
    [FieldOffset(16)] public uint Flags;
  }
  [DllImport("kernel32.dll", EntryPoint="WriteConsoleInputW")] public static extern bool WriteMouse(IntPtr h, MOUSE_REC[] r, uint n, out uint w);
  static IntPtr outH, inH; static INFO info;
  public static bool Attach(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    outH = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    inH = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    return GetConsoleScreenBufferInfo(outH, out info);
  }
  public static int Rows { get { return info.Win.B - info.Win.T + 1; } }
  public static string Row(int r, out short[] attrs) {
    short w = info.Size.X;
    var buf = new CHAR_INFO[w];
    var rect = new SMALL_RECT { L = 0, T = (short)(info.Win.T + r), R = (short)(w - 1), B = (short)(info.Win.T + r) };
    ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
    var sb = new StringBuilder();
    attrs = new short[w];
    for (int i = 0; i < w; i++) { sb.Append(buf[i].Ch); attrs[i] = buf[i].Attr; }
    return sb.ToString();
  }
  // buttons: 1 = left button down. ctrl: dwControlKeyState (0x2 = LEFT_ALT, 0x10 = SHIFT).
  public static void Mouse(int col, int row, uint buttons, uint ctrl) {
    var p = new COORD { X = (short)col, Y = (short)(info.Win.T + row) };
    var recs = new MOUSE_REC[] { new MOUSE_REC { EventType = 2, Pos = p, Buttons = buttons, Ctrl = ctrl, Flags = 0 } };
    uint n; WriteMouse(inH, recs, 1, out n);
  }
}
"@
  $ALT = 0x2; $SHIFT = 0x10
  $log = New-Object Collections.Generic.List[string]
  $script:fails = 0
  function Check($ok, $msg) { if ($ok) { $log.Add("  ok   $msg") } else { $log.Add("  FAIL $msg"); $script:fails++ } }
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }

  # Where the numbered lines are: screen row and the column of "LN" for each of them.
  $line = @{}
  $col0 = -1
  for ($r = 0; $r -lt [K]::Rows; $r++) {
    $a = $null
    $t = [K]::Row($r, [ref]$a)
    if ($t -match 'LN(\d\d)-0123') { $line[[int]$Matches[1]] = $r; $col0 = $t.IndexOf('LN') }
  }
  if ($line.Count -lt 12 -or $col0 -lt 0) { "numbered lines not on screen (found $($line.Count))" | Set-Content $Out -Encoding UTF8; exit 1 }
  $log.Add("lines LN00..LN13 at screen rows $($line[0])..$($line[13]), text starts at col $col0")

  function Attrs($n) { $a = $null; [void][K]::Row($line[$n], [ref]$a); , $a }
  $base = @{}
  foreach ($n in $line.Keys) { $base[$n] = Attrs $n }
  # Is the cell at (line n, text offset off) highlighted?
  function Hi($n, $off) { (Attrs $n)[$col0 + $off] -ne $base[$n][$col0 + $off] }
  # Highlighted offsets 0..42 of line n, as "first-last" (or "none").
  function Span($n) {
    $a = Attrs $n
    $on = @(0..42 | Where-Object { $a[$col0 + $_] -ne $base[$n][$col0 + $_] })
    if ($on.Count -eq 0) { 'none' } elseif ($on.Count -eq ($on[-1] - $on[0] + 1)) { "$($on[0])-$($on[-1])" } else { "gaps($($on -join ','))" }
  }
  function Click($n, $off, $ctrl) {
    [K]::Mouse($col0 + $off, $line[$n], 1, $ctrl); Start-Sleep -Milliseconds 120
    [K]::Mouse($col0 + $off, $line[$n], 0, $ctrl); Start-Sleep -Milliseconds 400
  }
  function Spans($from, $to) { ($from..$to | ForEach-Object { "LN{0:d2}={1}" -f $_, (Span $_) }) -join ' ' }

  if ($MouseMode) {
    $log.Add("0. held plain press in a mouse-mode pane")
    [K]::Mouse($col0 + 20, $line[12], 1, 0); Start-Sleep -Milliseconds 400
    Check (-not (Hi 12 20)) "the pressed cell is not highlighted (press held back for the app)"
    [K]::Mouse($col0 + 20, $line[12], 0, 0); Start-Sleep -Milliseconds 400
  } else {
    $log.Add("0. held plain press in a plain pane")
    [K]::Mouse($col0 + 20, $line[12], 1, 0); Start-Sleep -Milliseconds 400
    Check (Hi 12 20) "the pressed cell is highlighted while the button is down"
    [K]::Mouse($col0 + 20, $line[12], 0, 0); Start-Sleep -Milliseconds 400
  }

  $log.Add("1. plain click on LN02+5")
  Click 2 5 0
  $log.Add("     " + (Spans 1 3))
  Check ((Spans 0 13) -notmatch '=\d|gaps') "nothing stays selected"

  $log.Add("2. Alt+click on LN08+10")
  Click 8 10 $ALT
  $log.Add("     " + (Spans 1 9))
  Check ((Span 1) -eq 'none') "LN01 not selected"
  Check ((Span 2) -eq '5-42') "LN02 selected from offset 5"
  Check (@(3..7 | Where-Object { (Span $_) -ne '0-42' }).Count -eq 0) "LN03..LN07 fully selected"
  Check ((Span 8) -eq '0-10') "LN08 selected up to offset 10"
  Check ((Span 9) -eq 'none') "LN09 not selected"

  $log.Add("3. Shift+click on LN10+3")
  Click 10 3 $SHIFT
  $log.Add("     " + (Spans 1 11))
  Check ((Span 2) -eq '5-42') "start stays at LN02 offset 5"
  Check ((Span 9) -eq '0-42') "LN09 fully selected"
  Check ((Span 10) -eq '0-3') "end moved to LN10 offset 3"
  Check ((Span 11) -eq 'none') "LN11 not selected"

  $log.Add("4. Alt+click on LN05+7 (inside the selection)")
  Click 5 7 $ALT
  $log.Add("     " + (Spans 1 7))
  Check ((Span 2) -eq '5-42') "start stays at LN02 offset 5"
  Check ((Span 5) -eq '0-7') "end moved back to LN05 offset 7"
  Check ((Span 6) -eq 'none') "LN06 no longer selected"

  $log.Add("5. Alt+click on LN00+2 (before the selection)")
  Click 0 2 $ALT
  $log.Add("     " + (Spans 0 6))
  Check ((Span 0) -eq '2-42') "selection now starts at LN00 offset 2"
  Check ((Span 2) -eq '0-42') "LN02 fully selected"
  Check ((Span 5) -eq '0-7') "the old end LN05 offset 7 is kept"
  Check ((Span 6) -eq 'none') "LN06 not selected"

  $log.Add("6. plain click on LN03+1")
  Click 3 1 0
  $log.Add("     " + (Spans 0 6))
  Check ((Spans 0 13) -notmatch '=\d|gaps') "selection cleared"

  [void][K]::FreeConsole()
  $log.Add("inner fails=$($script:fails)")
  [IO.File]::WriteAllLines($Out, $log, (New-Object Text.UTF8Encoding $false))
  exit 0
}

# Do not leak this Claude session's env into the throwaway ccnest.
Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-click-extend"
New-Item -ItemType Directory -Force $work | Out-Null
$report = Join-Path $work 'inner.txt'
Remove-Item $report -ErrorAction SilentlyContinue
$shim = Join-Path $work 'claude-shim.cmd'
$shimPs = Join-Path $PSScriptRoot 'click-extend-selection-shim.ps1'
$shimArgs = if ($MouseMode) { ' -MouseMode' } else { '' }
Set-Content $shim "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`"$shimArgs" -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$env:CCNEST_INPUT_TRACE = '1'
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$traceFrom = if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 }

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
$fails = 1
try {
  Start-Sleep -Seconds 6
  $c = Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1
  if (-not $c) { throw "no ccnest.exe under conhost $($p.Id)" }
  $innerArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', '-ProcId', $c.ProcessId, '-Out', "`"$report`"")
  if ($MouseMode) { $innerArgs += '-MouseMode' }
  Start-Process powershell.exe -ArgumentList $innerArgs -WindowStyle Hidden -Wait
  $lines = @(Get-Content $report -Encoding UTF8)
  $lines
  $fails = if ($lines[-1] -match '^inner fails=(\d+)$') { [int]$Matches[1] } else { 1 }
  Start-Sleep -Milliseconds 500
  $ext = @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $traceFrom | Where-Object { $_ -match 'extend_selection' })
  "trace:"
  $ext | ForEach-Object { "  " + ($_ -replace '^\S+\s+', '') }
  if ($ext.Count -ne 4) { "  FAIL expected 4 extend_selection lines, got $($ext.Count)"; $fails++ } else { "  ok   4 extend_selection lines" }
} finally {
  Kill-Tree $p.Id
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
$mode = if ($MouseMode) { 'mouse-mode pane' } else { 'plain pane' }
if ($fails -eq 0) { "PASS ($mode)" } else { "FAIL ($mode): $fails check(s)"; exit 1 }
