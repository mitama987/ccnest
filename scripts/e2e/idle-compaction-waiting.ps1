# v0.1.43: a waiting line followed by idle-compaction lines is still "background".
# (ASCII only: PowerShell 5.1 reads a BOM-less script as ANSI. The screens are in
# fixtures/idle-compaction-step*.txt, UTF-8.)
#
# A disposable ccnest in conhost runs a fake claude (idle-compaction-shim.ps1) that
# draws the screen from the user's 2026-11-18 screenshot, so the text goes through the
# real path: ConPTY -> vt100 -> Screen::rows -> status::screen_text -> detect_status.
# The verdict is read from CCNEST_STATUS_TRACE.
#  1. waiting line + child lines + "Compacted while idle" notice  -> Background
#  2. a new turn below the notice (the workflow finished)          -> NeedsAttention
# The fixtures are transcribed from a screenshot, not captured from Claude Code
# (the real screen needs a workflow left idle for an hour). No keys are injected and
# the user's own ccnest is not touched.
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe')
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP 'ccnest-e2e-idle-compaction'
New-Item -ItemType Directory -Force $work | Out-Null
$trace = Join-Path $work 'trace.txt'
$flag = Join-Path $work 'next.flag'
Remove-Item $trace, $flag -ErrorAction SilentlyContinue
$shim = Join-Path $work 'claude-shim.cmd'
$shimPs = Join-Path $PSScriptRoot 'idle-compaction-shim.ps1'
$fixtures = Join-Path $PSScriptRoot 'fixtures'
# `title claude` = the generic title a real claude shows first (not taken as a task name).
Set-Content $shim @(
  '@echo off',
  'title claude',
  "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`" -Dir `"$fixtures`" -Flag `"$flag`""
) -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim
$env:CCNEST_STATUS_TRACE = $trace

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Trace { if (Test-Path $trace) { try { [IO.File]::ReadAllText($trace, [Text.Encoding]::UTF8) } catch { '' } } else { '' } }
function Status($t) { if ($t -match 'status=(\w+)') { $Matches[1] } else { '' } }
function Wait-Trace([scriptblock]$cond, [int]$sec) {
  $until = (Get-Date).AddSeconds($sec)
  while ((Get-Date) -lt $until) { $t = Trace; if ($t -and (& $cond $t)) { return $t }; Start-Sleep -Milliseconds 500 }
  return (Trace)
}
function Show-Trace($t) { foreach ($line in ($t -split "`r?`n")) { "    | $line" } }
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }

$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = Start-Process -FilePath 'conhost.exe' -ArgumentList "`"$exe`"" -WorkingDirectory $work -PassThru
try {
  "1. waiting line + child lines + idle-compaction notice"
  $t = Wait-Trace { param($t) $t.Contains('Compacted while idle') -and $t.Contains('affilia-xboard-gap') } 40
  Start-Sleep -Seconds 3
  $t = Trace
  Show-Trace $t
  Check ($t -match '(?m)^\S Waiting for 1 dynamic workflow to finish$') "the waiting line is on the screen, at column 0"
  Check ($t -match '(?m)^  \S+\s+Skills restored') "its child lines are indented"
  Check ($t -match '(?m)^\S Compacted while idle, before the prompt cache expired') "the notice is below it, at column 0"
  Check ($t -match '(?m)^  "idleCompaction": false in settings\.json$') "the wrapped rest of the notice is indented"
  Check ((Status $t) -eq 'Background') "status=Background (was NeedsAttention up to v0.1.42): $(Status $t)"

  "2. a new turn below the notice"
  Set-Content $flag 'go' -Encoding ASCII
  $t = Wait-Trace { param($t) $t.Contains('Churned for 15s') } 30
  Start-Sleep -Seconds 3
  $t = Trace
  Show-Trace $t
  Check ($t.Contains('Compacted while idle') -and $t.Contains('Waiting for 1 dynamic workflow to finish')) "the old waiting line and the notice are still on the screen"
  Check ((Status $t) -eq 'NeedsAttention') "status=NeedsAttention: $(Status $t)"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
