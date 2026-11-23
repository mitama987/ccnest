# v0.1.53: two tab-colour fixes, checked on the real binary.
#  1. A pane scrolled up into its history is still judged on its bottom screen
#     (up to v0.1.52 the verdict froze at the last bottom view = green while busy).
#  2. A turn that ends with background work (footer pill "1 shell") while you are on another
#     tab stays "needs you" (yellow) until you open that tab; after that it is green while the
#     background work runs. A turn that ends while you watch the tab is green right away.
# A throw-away ccnest runs with no console window (CREATE_NO_WINDOW); the fake claude is
# turn-end-shim.ps1 (3 tabs: alpha, bravo, cc). Keys and the mouse wheel go in through
# WriteConsoleInputW (inner mode of dblclick-rename-tab.ps1), the verdict is read from
# CCNEST_STATUS_TRACE (status = shown, detected = read from the screen, unseen, scroll).
# The user's own ccnest windows and keyboard are not touched. No real claude is started.
#
#  A. all three tabs busy; on cc (active) turn the wheel up -> scroll > 0, still Busy
#  B. cc's turn ends (no pill) while still scrolled up -> NeedsAttention, scroll still > 0
#  C. go to bravo; alpha and bravo end their turns with "1 shell"
#     -> alpha (not watched): shown NeedsAttention, detected Background, unseen
#     -> bravo (watched):     shown Background
#  D. open alpha -> Background, not unseen
#  E. back to bravo -> alpha stays Background (it does not turn yellow again)
# ASCII only (no BOM needed).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe')
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$shimPs = Join-Path $PSScriptRoot 'turn-end-shim.ps1'
$tag = 'ccnest-e2e-unseen-' + (Get-Date -Format 'HHmmss')
$work = Join-Path $env:TEMP $tag
New-Item -ItemType Directory -Force $work | Out-Null
$trace = Join-Path $work 'status-trace.txt'
$shim = Join-Path $work 'claude-shim.cmd'
Set-Content $shim @(
  '@echo off',
  "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`" -Work `"$work`""
) -Encoding ASCII
$run = Join-Path $work 'run.cmd'
Set-Content $run @(
  '@echo off', 'set CCNEST_PID=', 'set CCNEST_JP_TITLE=off', 'set CCNEST_USAGE=off', 'set CCNEST_TAB_GROUP=off',
  "set CCNEST_TAB_NAMES=$(Join-Path $work 'tab-names.json')", "set CCNEST_TAB_HOLDS=$(Join-Path $work 'tab-holds.json')",
  "set CCNEST_STATUS_TRACE=$trace", "set CCNEST_CLAUDE_BIN=$shim",
  "`"$exe`""
) -Encoding ASCII
$screenFile = Join-Path $work 'screen.txt'

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Send($ccId, $ops) {
  if (-not $ops) { $ops = 'sleep:1' }
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"") -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 2) { throw "inner failed: $($l -join ' ')" }
  ($l[0] -replace "`0", '').TrimEnd()
}
# Latest trace -> @{ 1 = @{ Status; Detected; Unseen; Scroll }; 2 = ...; 3 = ... } (key = tab N).
function Panes {
  $res = @{}
  if (-not (Test-Path $trace)) { return $res }
  $t = ''
  try { $t = [IO.File]::ReadAllText($trace, [Text.Encoding]::UTF8) } catch { return $res }
  foreach ($block in ($t -split '(?m)^=== pane ' | Where-Object { $_ })) {
    if ($block -notmatch 'status=(\w+) detected=(\w+) unseen=(\w+) scroll=(\d+)') { continue }
    $h = @{ Status = $Matches[1]; Detected = $Matches[2]; Unseen = $Matches[3]; Scroll = [int]$Matches[4] }
    if ($block -match 'tab(\d+)') { $res[[int]$Matches[1]] = $h }
  }
  $res
}
function Show($p) { (1..3 | ForEach-Object { $x = $p[$_]; if ($x) { "tab$_ $($x.Status)/$($x.Detected) unseen=$($x.Unseen) scroll=$($x.Scroll)" } else { "tab$_ -" } }) -join ' | ' }
# Wait (up to $sec s) until $test (taking the Panes table) is true; returns the table.
function Wait-For($sec, $test) {
  $p = @{}
  $deadline = (Get-Date).AddSeconds($sec)
  do { $p = Panes; if (& $test $p) { break }; Start-Sleep -Milliseconds 700 } while ((Get-Date) -lt $deadline)
  $p
}

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$proc = $null; $cc = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
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
  Start-Sleep -Seconds 5
  foreach ($i in 1..2) { [void](Send $cc 'ctrl:t'); Start-Sleep -Seconds 4 }
  $p = Wait-For 20 { param($x) $x[1] -and $x[2] -and $x[3] -and $x[1].Status -eq 'Busy' -and $x[2].Status -eq 'Busy' -and $x[3].Status -eq 'Busy' }
  "   start: " + (Show $p)
  Check ($p[3] -and $p[3].Status -eq 'Busy' -and $p[1] -and $p[2]) "three tabs, all busy"

  "A. on cc (active), turn the wheel up"
  $row = Send $cc 'wheelup:60,10,8'
  $p = Wait-For 8 { param($x) $x[3] -and $x[3].Scroll -gt 0 }
  "   " + (Show $p)
  Check ($p[3].Scroll -gt 0) "cc is scrolled into its history (scroll=$($p[3].Scroll))"
  Check ($p[3].Status -eq 'Busy') "still Busy while busy (status=$($p[3].Status))"

  "B. cc's turn ends (no background) while scrolled up"
  New-Item -ItemType File (Join-Path $work 'done3') | Out-Null
  $p = Wait-For 10 { param($x) $x[3].Status -eq 'NeedsAttention' }
  "   " + (Show $p)
  Check ($p[3].Status -eq 'NeedsAttention') "cc turns NeedsAttention without scrolling down (was stuck at Busy up to v0.1.52)"
  Check ($p[3].Scroll -gt 0) "and the pane is still scrolled up (scroll=$($p[3].Scroll))"

  "C. go to bravo; alpha and bravo end their turns with '1 shell'"
  [void](Send $cc 'altleft')
  Start-Sleep -Seconds 3   # one tick on bravo first (bravo = viewed)
  foreach ($k in 'bg1', 'bg2', 'done1', 'done2') { New-Item -ItemType File (Join-Path $work $k) | Out-Null }
  $p = Wait-For 10 { param($x) $x[1].Detected -eq 'Background' -and $x[2].Detected -eq 'Background' }
  "   " + (Show $p)
  Check ($p[1].Detected -eq 'Background' -and $p[1].Status -eq 'NeedsAttention' -and $p[1].Unseen -eq 'true') "alpha (not watched): shown NeedsAttention, detected Background, unseen"
  Check ($p[2].Detected -eq 'Background' -and $p[2].Status -eq 'Background' -and $p[2].Unseen -eq 'false') "bravo (watched): shown Background"
  Start-Sleep -Seconds 4
  $p = Panes
  Check ($p[1].Status -eq 'NeedsAttention') "alpha stays NeedsAttention over the next ticks (status=$($p[1].Status))"

  "D. open alpha"
  [void](Send $cc 'altleft')
  $p = Wait-For 8 { param($x) $x[1].Status -eq 'Background' }
  "   " + (Show $p)
  Check ($p[1].Status -eq 'Background' -and $p[1].Unseen -eq 'false') "alpha: Background, not unseen"

  "E. back to bravo"
  [void](Send $cc 'altright')
  Start-Sleep -Seconds 5
  $p = Panes
  "   " + (Show $p)
  Check ($p[1].Status -eq 'Background') "alpha stays Background (status=$($p[1].Status))"
  Check ($p[3].Status -eq 'NeedsAttention') "cc still NeedsAttention"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc }
  if ($proc) { Kill-Tree $proc.Id }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
