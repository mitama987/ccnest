# v0.1.54: a running agent row below the footer is "background" (green), even when the
# turn ended without the "Waiting for 1 background agent to finish" line; and a tab put
# on hold (Alt+C) in that state keeps its claude running (the background agent with it).
# (ASCII only: PowerShell 5.1 reads a BOM-less script as ANSI. The screens are in
# fixtures/agent-list-step*.txt, UTF-8.)
#
# A disposable ccnest with no console window (CREATE_NO_WINDOW) runs a fake claude
# (idle-compaction-shim.ps1) that draws the screen of the user's 2026-11-24 screenshot,
# so the text goes through the real path: ConPTY -> vt100 -> Screen::rows ->
# status::screen_text -> detect_status. The verdict is read from CCNEST_STATUS_TRACE,
# the hold from CCNEST_INPUT_TRACE. Alt+C goes in through WriteConsoleInputW (inner mode of
# dblclick-rename-tab.ps1). The user's own ccnest and keyboard are not touched.
#  1. no waiting line, no pill, an agent row below the footer -> Background
#     Alt+C -> kept running (stopped=0 kept=1); Alt+C again releases the hold
#  2. the agent finished (the list is gone), the notified turn ended -> NeedsAttention
#     Alt+C -> stopped (stopped=1 kept=0): the guard does not block the normal case
# Step 1 is transcribed from a screenshot; the row and the step 2 footer have the shape
# captured from Claude Code v2.1.331 (agent-list-real.ps1).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe')
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$shimPs = Join-Path $PSScriptRoot 'idle-compaction-shim.ps1'
$fixtures = Join-Path $PSScriptRoot 'fixtures'
$work = Join-Path $env:TEMP ('ccnest-e2e-agent-list-' + (Get-Date -Format 'HHmmss'))
New-Item -ItemType Directory -Force $work | Out-Null
$trace = Join-Path $work 'status-trace.txt'
$flag = Join-Path $work 'next.flag'
$screenFile = Join-Path $work 'screen.txt'
$inputTrace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$shim = Join-Path $work 'claude-shim.cmd'
# `title claude` = the generic title a real claude shows first (not taken as a task name).
Set-Content $shim @(
  '@echo off',
  'title claude',
  "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`" -Dir `"$fixtures`" -Flag `"$flag`" -Step1 agent-list-step1.txt -Step2 agent-list-step2.txt"
) -Encoding ASCII
$run = Join-Path $work 'run.cmd'
Set-Content $run @(
  '@echo off', 'set CCNEST_PID=', 'set CCNEST_JP_TITLE=off', 'set CCNEST_USAGE=off', 'set CCNEST_INPUT_TRACE=1',
  "set CCNEST_TAB_NAMES=$(Join-Path $work 'tab-names.json')", "set CCNEST_TAB_HOLDS=$(Join-Path $work 'tab-holds.json')",
  "set CCNEST_STATUS_TRACE=$trace", "set CCNEST_CLAUDE_BIN=$shim",
  "`"$exe`""
) -Encoding ASCII

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id, $all) { $all | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId $all } }
# Is the fake claude (the powershell that runs the shim) alive under this ccnest?
function Shim-Alive($ccId) { $all = @(Get-CimInstance Win32_Process); [bool](@(Descendants $ccId $all | Where-Object { $_.CommandLine -and $_.CommandLine.Contains('idle-compaction-shim.ps1') }).Count) }
function Trace { if (Test-Path $trace) { try { [IO.File]::ReadAllText($trace, [Text.Encoding]::UTF8) } catch { '' } } else { '' } }
function Field($t, $name) { if ($t -match "$name=(\w+)") { $Matches[1] } else { '' } }
function Wait-Trace([scriptblock]$cond, [int]$sec) {
  $until = (Get-Date).AddSeconds($sec)
  while ((Get-Date) -lt $until) { $t = Trace; if ($t -and (& $cond $t)) { return $t }; Start-Sleep -Milliseconds 500 }
  return (Trace)
}
function Show-Trace($t) { foreach ($line in ($t -split "`r?`n")) { "    | $line" } }
function Send($ccId, $ops) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"") -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 2) { throw "inner failed: $($l -join ' ')" }
}
function TraceLen { if (Test-Path $inputTrace) { @(Get-Content $inputTrace -Encoding UTF8).Count } else { 0 } }
function TraceSince($from, $pattern) { @(Get-Content $inputTrace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match $pattern } | ForEach-Object { [void]($_ -match $pattern); $Matches[0] }) }
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
$HOLD = 'hold_tab idx=0 held=\w+ active=0 stopped=\d+ kept=\d+'

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$proc = $null; $cc = $null
try {
  $si = New-Object Diagnostics.ProcessStartInfo 'cmd.exe', "/c `"$run`""
  $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.WorkingDirectory = $work
  $proc = [Diagnostics.Process]::Start($si)
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $cc = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1).Id
  } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw "no new ccnest.exe" }

  "1. an agent row below the footer, no waiting line, no pill"
  $t = Wait-Trace { param($t) $t.Contains('116.0k tokens') -and $t.Contains('to manage') } 40
  Start-Sleep -Seconds 3
  $t = Trace
  Show-Trace $t
  Check ($t -notmatch 'Waiting for') "there is no waiting line on the screen"
  Check ($t -match '(?m)^  \S general-purpose\s.*\s2m 17s \S \S 116\.0k tokens$') "the agent row is below the footer (elapsed + tokens)"
  Check ((Field $t 'detected') -eq 'Background') "detected=Background (was NeedsAttention up to v0.1.53): $(Field $t 'detected')"
  Check ((Field $t 'status') -eq 'Background') "status=Background (the tab shows green): $(Field $t 'status')"

  "1b. Alt+C on that tab: claude keeps running"
  $n0 = TraceLen
  Send $cc 'alt:c'
  $h = TraceSince $n0 $HOLD
  "   trace: $($h -join ' / ')"
  Check (($h -join '') -match 'held=true active=0 stopped=0 kept=1') "hold_tab ... stopped=0 kept=1 (was stopped=1 up to v0.1.53)"
  Start-Sleep -Seconds 3
  Check (Shim-Alive $cc) "the fake claude is still running"
  Send $cc 'alt:c'
  Start-Sleep -Seconds 1

  "2. the agent finished: the list is gone, the notified turn ended"
  Set-Content $flag 'go' -Encoding ASCII
  $t = Wait-Trace { param($t) $t.Contains('Churned for 15s') } 30
  Start-Sleep -Seconds 3
  $t = Trace
  Show-Trace $t
  Check ($t -notmatch 'general-purpose') "the agent list is gone"
  Check ((Field $t 'detected') -eq 'NeedsAttention') "detected=NeedsAttention: $(Field $t 'detected')"

  "2b. Alt+C now: claude is stopped (the guard does not block the normal case)"
  Start-Sleep -Seconds 3
  $n0 = TraceLen
  Send $cc 'alt:c'
  $h = TraceSince $n0 $HOLD
  "   trace: $($h -join ' / ')"
  Check (($h -join '') -match 'held=true active=0 stopped=1 kept=0') "hold_tab ... stopped=1 kept=0"
  Start-Sleep -Seconds 3
  Check (-not (Shim-Alive $cc)) "the fake claude is gone"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc }
  if ($proc) { Kill-Tree $proc.Id }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
