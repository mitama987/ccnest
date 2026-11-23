# `ccnest --close [--after <sec>]` (v0.1.51): the tab of the pane that typed the command closes.
# Real check with a disposable ccnest started under conhost (fake claude = a cmd that logs
# CCNEST_PID / CCNEST_PANE / args and waits). The close command is run as a separate process
# with CCNEST_PID / CCNEST_PANE set the way ccnest sets them for a pane's children.
#  1. the 3 panes got CCNEST_PID=<window pid> and a distinct CCNEST_PANE
#  2. --close --after 4 from tab A: returns at once (exit 0), the tab is still there after ~1.5 s,
#     gone within ~7 s (the window checks on a 2 s tick) with its whole process tree; the other
#     tabs stay; the tab bar loses its name
#  3. --close twice for tab B (--after 20, then --after 2): the later deadline wins
#  4. no CCNEST_PANE / no CCNEST_PID -> fails at once (exit 1) and says which variable
#  5. a pane id the window does not have -> error, the window keeps running
#  6. the last tab of the window is refused (exit 1) and stays open
#  7. a pid that is not running -> fails at once
# This script is ASCII only on purpose (no BOM needed). It never touches the user's ccnest and
# injects no keys (the screen is only read).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Inner, [int]$ProcId, [string]$Out
)
$ErrorActionPreference = 'Stop'

if ($Inner) {
  trap { ("inner error: " + $_) | Set-Content $Out -Encoding UTF8; exit 1 }
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
  static IntPtr outH; static INFO info;
  public static bool Attach(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    outH = CreateFileW("CONOUT$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    return GetConsoleScreenBufferInfo(outH, out info);
  }
  public static string Screen() {
    var sb = new StringBuilder();
    short w = info.Size.X;
    for (int r = info.Win.T; r <= info.Win.B; r++) {
      var buf = new CHAR_INFO[w];
      var rect = new SMALL_RECT { L = 0, T = (short)r, R = (short)(w - 1), B = (short)r };
      ReadConsoleOutputW(outH, buf, new COORD { X = w, Y = 1 }, new COORD { X = 0, Y = 0 }, ref rect);
      foreach (var c in buf) if ((c.Attr & 0x200) == 0) sb.Append(c.Ch);
      sb.Append('\n');
    }
    return sb.ToString();
  }
}
"@
  if (-not [K]::Attach([uint32]$ProcId)) { "attach failed" | Set-Content $Out -Encoding UTF8; exit 1 }
  $text = [K]::Screen()
  [void][K]::FreeConsole()
  [IO.File]::WriteAllText($Out, $text, (New-Object Text.UTF8Encoding $false))
  exit 0
}

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
# Nested (typed inside a ccnest tab): the target must be the disposable window only.
Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue
Remove-Item env:CCNEST_PANE -ErrorAction SilentlyContinue
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-close-tab"
New-Item -ItemType Directory -Force $work | Out-Null
$shimLog = Join-Path $work 'shim.log'
Remove-Item $shimLog -ErrorAction SilentlyContinue
$shim = Join-Path $work 'claude-shim.cmd'
# Fake claude: log "<CCNEST_PID> <CCNEST_PANE> <args>", print the args, wait.
# `title claude` = the generic title a real claude sets at start (ccnest ignores it as a task
# name, so the given tab names stay on the tab bar).
Set-Content $shim @(
  '@echo off',
  'title claude',
  "echo %CCNEST_PID% %CCNEST_PANE% %*>>`"$shimLog`"",
  'echo SHIM %*',
  'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600'
) -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$screenFile = Join-Path $work 'screen.txt'

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function Shims($rootId, $flag) {
  @(Descendants $rootId | Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -match "$flag\s+([0-9a-f-]{36})" } | ForEach-Object { [void]($_.CommandLine -match "$flag\s+([0-9a-f-]{36})"); $Matches[1] })
}
function AllShims($rootId) { @(Shims $rootId '--session-id') + @(Shims $rootId '--resume') }
function SleepProcs($rootId) { @(Descendants $rootId | Where-Object { $_.Name -eq 'powershell.exe' -and $_.CommandLine -match 'Start-Sleep -Seconds 600' }).Count }
function Screen($ccId) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Inner', 'screen', '-ProcId', $ccId, '-Out', "`"$screenFile`"") -WindowStyle Hidden -Wait
  Get-Content $screenFile -Raw -Encoding UTF8
}
function Wait-Until($sec, $cond) { $until = (Get-Date).AddSeconds($sec); while ((Get-Date) -lt $until) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 300 }; return $false }
# Run a ccnest command with the given environment (null = variable not set) and return
# (Code, Out, Err, Sec).
function Run-Ccnest($argList, $pidEnv, $paneEnv) {
  $o = Join-Path $work 'cmd.out'; $e = Join-Path $work 'cmd.err'
  $oldPid = $env:CCNEST_PID; $oldPane = $env:CCNEST_PANE
  if ($null -ne $pidEnv) { $env:CCNEST_PID = "$pidEnv" } else { Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue }
  if ($null -ne $paneEnv) { $env:CCNEST_PANE = "$paneEnv" } else { Remove-Item env:CCNEST_PANE -ErrorAction SilentlyContinue }
  try {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $exe -ArgumentList $argList -NoNewWindow -Wait -PassThru -RedirectStandardOutput $o -RedirectStandardError $e
    $sw.Stop()
  } finally {
    if ($null -ne $oldPid) { $env:CCNEST_PID = $oldPid } else { Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue }
    if ($null -ne $oldPane) { $env:CCNEST_PANE = $oldPane } else { Remove-Item env:CCNEST_PANE -ErrorAction SilentlyContinue }
  }
  $r = [pscustomobject]@{ Code = $proc.ExitCode; Out = (Get-Content $o -Raw -Encoding UTF8); Err = (Get-Content $e -Raw -Encoding UTF8); Sec = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
  "  $ ccnest $($argList -join ' ')  [PID=$pidEnv PANE=$paneEnv]"
  "    exit=$($r.Code) ($($r.Sec)s)"
  foreach ($line in (("$($r.Out)`n$($r.Err)") -split "`r?`n" | Where-Object { $_.Trim() })) { "    | $line" }
  $script:last = $r
}
# The pane id (2nd field) the fake claude logged for the session id.
function PaneOf($sid) {
  foreach ($l in @(Get-Content $shimLog)) { $f = $l -split ' '; if ($l -match [regex]::Escape($sid)) { return $f[1] } }
  return $null
}

$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
# No console window at all (CREATE_NO_WINDOW): nothing shows up and nothing takes the keyboard
# focus from the user. The screen is read through the console API, which needs no window.
$si = New-Object Diagnostics.ProcessStartInfo 'cmd.exe', "/c `"$exe`""
$si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.WorkingDirectory = $work
$p = [Diagnostics.Process]::Start($si)
$logDir = $null
$inbox = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  Start-Sleep -Seconds 6
  $cc = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1).ProcessId
  if (-not $cc) { throw "no ccnest.exe under conhost $($p.Id)" }
  $id1 = @(Shims $cc '--session-id')[0]
  "disposable ccnest pid: $cc / tab1 session: $id1"
  $inbox = Join-Path $env:APPDATA "ccnest\requests\$cc"

  # Two more tabs (sessions with a conversation log; no user turn in the log so the last
  # message does not become the task name and override the tab name).
  $a = [guid]::NewGuid().ToString(); $b = [guid]::NewGuid().ToString()
  $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
  $logDir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
  New-Item -ItemType Directory -Force $logDir | Out-Null
  foreach ($s in @($a, $b)) {
    [IO.File]::WriteAllText((Join-Path $logDir "$s.jsonl"), ('{"type":"file-history-snapshot","sessionId":"' + $s + '"}' + "`n"), (New-Object Text.UTF8Encoding $false))
  }
  $o = Join-Path $work 'add.out'
  [void](Start-Process -FilePath $exe -ArgumentList @('--add', '--to', $cc, '--resume', "$a=tab-a", '--resume', "$b=tab-b") -NoNewWindow -Wait -PassThru -RedirectStandardOutput $o)
  $opened = Wait-Until 14 { $r = @(Shims $cc '--resume'); ($r -contains $a) -and ($r -contains $b) }
  Start-Sleep -Seconds 1
  "setup: tabs added with --add (both started: $opened)"
  $pane1 = PaneOf $id1; $paneA = PaneOf $a; $paneB = PaneOf $b
  "  panes: tab1=$pane1 tab-a=$paneA tab-b=$paneB"
  Check ($opened -and $pane1 -and $paneA -and $paneB) "three tabs are open and each pane logged its CCNEST_PANE"
  Check ((@(Get-Content $shimLog | ForEach-Object { ($_ -split ' ')[0] } | Sort-Object -Unique) -join ',') -eq "$cc") "every pane got CCNEST_PID=$cc"
  Check ((@($pane1, $paneA, $paneB | Sort-Object -Unique)).Count -eq 3) "the three panes have distinct CCNEST_PANE values"
  $sleepBefore = SleepProcs $cc
  $tabRow = ((Screen $cc) -split "`n")[0]
  Check (($tabRow -match 'tab-a') -and ($tabRow -match 'tab-b')) "tab bar shows tab-a and tab-b (row: $($tabRow.TrimEnd()))"

  "2. --close --after 4 from tab-a"
  $t0 = Get-Date
  Run-Ccnest @('--close', '--after', '4') $cc $paneA
  Check ($last.Code -eq 0) "exit code 0"
  Check ($last.Sec -lt 3) "returns at once (does not wait for the tab to close)"
  Check ($last.Out -match '(?<!\d)4 ') "the reply says 4 seconds"
  Start-Sleep -Milliseconds 1200
  Check (@(Shims $cc '--resume') -contains $a) "1.5 s in: tab-a is still open (the last reply stays visible)"
  $gone = Wait-Until 10 { @(Shims $cc '--resume') -notcontains $a }
  $elapsed = ((Get-Date) - $t0).TotalSeconds
  "  tab-a gone after $([math]::Round($elapsed, 1)) s"
  Check $gone "tab-a closed (its claude shim is gone)"
  Check ($elapsed -ge 3.5 -and $elapsed -le 8.5) "closed after about --after seconds (4 s + up to one 2 s tick; measured $([math]::Round($elapsed, 1)) s)"
  Check ((@(Shims $cc '--resume') -contains $b) -and (@(Shims $cc '--session-id') -contains $id1)) "the other tabs (tab1, tab-b) are untouched"
  Check ((SleepProcs $cc) -eq ($sleepBefore - 1)) "the whole process tree of tab-a ended (one fewer waiting powershell)"
  $screen = Screen $cc
  $tabRow = ($screen -split "`n")[0]
  Check (($tabRow -notmatch 'tab-a') -and ($tabRow -match 'tab-b')) "tab bar lost tab-a and kept tab-b (row: $($tabRow.TrimEnd()))"

  "3. --close twice for tab-b: the later deadline wins"
  Run-Ccnest @('--close', '--after', '20') $cc $paneB
  Check ($last.Code -eq 0) "first request accepted (20 s)"
  Run-Ccnest @('--close', '--after', '2') $cc $paneB
  Check ($last.Code -eq 0) "second request accepted (2 s)"
  $t1 = Get-Date
  $gone = Wait-Until 10 { @(Shims $cc '--resume') -notcontains $b }
  $elapsed = ((Get-Date) - $t1).TotalSeconds
  Check ($gone -and $elapsed -lt 8) "tab-b closed within about 2 s + a tick, not after 20 s (measured $([math]::Round($elapsed, 1)) s)"
  Check (@(Shims $cc '--session-id') -contains $id1) "tab1 is untouched"

  "4. no pane / no pid mark"
  Run-Ccnest @('--close') $cc $null
  Check ($last.Code -eq 1 -and $last.Sec -lt 3) "no CCNEST_PANE: exit 1 at once"
  Check ($last.Err -match 'CCNEST_PANE') "the error names CCNEST_PANE"
  Run-Ccnest @('--close') $null $pane1
  Check ($last.Code -eq 1 -and $last.Sec -lt 3) "no CCNEST_PID: exit 1 at once"

  "5. a pane id the window does not have"
  Run-Ccnest @('--close', '--after', '0') $cc 99999
  Check ($last.Code -eq 1 -and $last.Err.Trim().Length -gt 0) "exit 1 with a reason"
  Start-Sleep -Seconds 3
  Check (@(Shims $cc '--session-id') -contains $id1) "the window and tab1 keep running"

  "6. the last tab is refused"
  Run-Ccnest @('--close', '--after', '0') $cc $pane1
  Check ($last.Code -eq 1 -and $last.Err.Trim().Length -gt 0) "exit 1 with a reason"
  Start-Sleep -Seconds 4
  Check ((@(Shims $cc '--session-id') -contains $id1) -and (Get-Process -Id $cc -ErrorAction SilentlyContinue)) "tab1 stays open and ccnest is still running"

  "7. a pid that is not running"
  $dead = (Start-Process cmd.exe -ArgumentList '/c', 'exit' -WindowStyle Hidden -PassThru -Wait).Id
  Run-Ccnest @('--close') $dead 1
  Check ($last.Code -eq 1 -and $last.Sec -lt 3) "exit 1 at once"
  Check ($last.Err -match "pid $dead") "the error names the pid"

  Check (@(Get-ChildItem $inbox -ErrorAction SilentlyContinue).Count -eq 0) "the request folder is empty again (no leftovers)"
  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  if ($logDir) { Remove-Item $logDir -Recurse -Force -ErrorAction SilentlyContinue }
  # A forced kill skips Drop, so the disposable window's request folder is removed here.
  if ($inbox) { Remove-Item $inbox -Recurse -Force -ErrorAction SilentlyContinue }
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
