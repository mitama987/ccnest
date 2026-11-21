# Holding a tab frees its memory (v0.1.49) with the REAL claude, once:
#  1. make a throwaway session headless (`claude -p`, light model)
#  2. reopen it in a disposable ccnest (no console window) and wait for the idle prompt;
#     type one more prompt and wait for the reply (the usual case: hold a tab after a turn).
#     The real screen text is printed: the judgement "safe to stop" reads this shape.
#  3. Alt+C -> the claude process tree is gone, its memory is freed
#  4. Enter -> claude is started again with --resume <same id>; the conversation is back
#  5. Alt+C twice (release / hold) -> a resumed session is stopped as well
# Keys go in through WriteConsoleInputW (inner mode of dblclick-rename-tab.ps1).
# -Work must be a folder claude already trusts (a new folder stops at the trust dialog).
# Uses a light model so the check does not eat the Fable allowance. ASCII only (PS 5.1, no BOM).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Work = (Join-Path $PSScriptRoot '..\..\target\e2e-hold-sleep'),
  [string]$Model = 'sonnet',
  [int]$WaitSec = 120
)
$ErrorActionPreference = 'Stop'

# Run from inside a Claude Code session, its env would switch transcript saving off in the child.
foreach ($k in @([Environment]::GetEnvironmentVariables('Process').Keys)) {
  if (($k -like 'CLAUDE*' -or $k -like 'AI_AGENT*' -or $k -like 'CCNEST_*') -and $k -ne 'CLAUDE_CONFIG_DIR' -and $k -ne 'CCNEST_CLAUDE_BIN') {
    [Environment]::SetEnvironmentVariable($k, $null, 'Process')
  }
}
$exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Work | Out-Null
$work = (Resolve-Path $Work).Path
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$statusTrace = Join-Path $work 'status-trace.txt'
$screenFile = Join-Path $work 'screen.txt'
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$env:CCNEST_INPUT_TRACE = '1'
$env:CCNEST_STATUS_TRACE = $statusTrace
$env:CCNEST_TAB_HOLDS = Join-Path $work 'tab-holds.json'
$env:CCNEST_TAB_NAMES = Join-Path $work 'tab-names.json'
$env:ANTHROPIC_MODEL = $Model
Remove-Item $statusTrace, $env:CCNEST_TAB_HOLDS -ErrorAction SilentlyContinue
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$claude = Join-Path $env:USERPROFILE '.local\bin\claude.exe'
if (-not (Test-Path $claude)) { $claude = (Get-Command claude).Source }

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id, $all) { $all | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId $all } }
# Everything ccnest started for its pane: @{ Count; Claude (claude.exe count); MB (private bytes) }.
function Tree($ccId) {
  $all = @(Get-CimInstance Win32_Process)
  $d = @(Descendants $ccId $all)
  @{ Count = $d.Count; Claude = @($d | Where-Object { $_.Name -eq 'claude.exe' }).Count; MB = [int](($d | Measure-Object PrivatePageCount -Sum).Sum / 1MB); Resume = @($d | Where-Object { $_.Name -eq 'claude.exe' -and $_.CommandLine -match "--resume\s+$sid" }).Count }
}
function Send($ccId, $ops) {
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"", '-AlsoRows', '1,-1') -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 4) { throw "inner failed: $($l -join ' ')" }
  @{ Text = ($l[0] -replace "`0", '').TrimEnd(); Frame = $l[2]; Bottom = $l[3] }
}
function StatusText { if (Test-Path $statusTrace) { try { [IO.File]::ReadAllText($statusTrace, [Text.Encoding]::UTF8) } catch { '' } } else { '' } }
function Wait-Status([scriptblock]$cond, [int]$sec) {
  $until = (Get-Date).AddSeconds($sec)
  while ((Get-Date) -lt $until) { $t = StatusText; if ($t -and (& $cond $t)) { return $t }; Start-Sleep -Milliseconds 700 }
  StatusText
}
function Show-Tail($t, $n) { @($t -split "`r?`n" | Where-Object { $_.Trim() }) | Select-Object -Last $n | ForEach-Object { "    | " + $(if ($_.Length -gt 110) { $_.Substring(0, 110) } else { $_ }) } }
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function TraceSince($from, $pattern) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match $pattern } | ForEach-Object { [void]($_ -match $pattern); $Matches[0] }) }
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
$ON_HOLD = -join (0x4FDD, 0x7559, 0x4E2D | ForEach-Object { [char]$_ })
$FREED = -join (0x30E1, 0x30E2, 0x30EA, 0x3092, 0x7A7A, 0x3051 | ForEach-Object { [char]$_ })

$sid = [guid]::NewGuid().ToString()
$enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
$logDir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
$log = Join-Path $logDir "$sid.jsonl"
$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$p = $null; $cc = $null
try {
  "1. make a throwaway session ($Model, headless)"
  Push-Location $work
  try { $out = & $claude -p --model $Model --session-id $sid "Reply with the single word BANANA." 2>&1 | Out-String } finally { Pop-Location }
  "  claude -p -> $(($out -replace '\s+', ' ').Trim())"
  Check (Test-Path $log) "the session has a conversation log"

  "2. reopen it in a disposable ccnest (no window), wait for the idle prompt"
  $si = New-Object Diagnostics.ProcessStartInfo 'cmd.exe', "/c `"`"$exe`" `"$work`" --resume $sid=hold-e2e`""
  $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.WorkingDirectory = $work
  $p = [Diagnostics.Process]::Start($si)
  $deadline = (Get-Date).AddSeconds(20)
  do { Start-Sleep -Milliseconds 500; $cc = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1).Id } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw "no new ccnest.exe" }
  $t = Wait-Status { param($t) $t.Contains('BANANA') -and $t -match 'status=NeedsAttention' } $WaitSec
  Start-Sleep -Seconds 4
  $t = StatusText
  "  the real screen (last lines):"
  Show-Tail $t 9
  Check ($t.Contains('BANANA') -and $t -match 'status=NeedsAttention') "the resumed conversation is on the screen, waiting for input"
  "2b. one real turn in the pane (typed), then wait for the reply"
  [void](Send $cc 'type:say only the word mango|enter')
  $t = Wait-Status { param($t) $t -match '(?m)^\S\s*mango\s*$' -and $t -match 'status=NeedsAttention' } $WaitSec
  # Past the "input was just sent" guard (5 s), and let any suggestion text appear.
  Start-Sleep -Seconds 8
  $t = StatusText
  "  the real screen after the turn (last lines):"
  Show-Tail $t 9
  Check ($t -match '(?m)^\S\s*mango\s*$' -and $t -match 'status=NeedsAttention') "claude answered and waits for input"
  $b = Tree $cc
  "  before: $($b.Count) processes under ccnest, claude.exe x$($b.Claude), $($b.MB) MB private"
  Check ($b.Claude -ge 1) "claude.exe is running"

  "3. Alt+C"
  $n0 = TraceLen
  $s = Send $cc 'alt:c'
  "  tab row: " + $s.Text
  "  bottom : " + $s.Bottom
  $h = TraceSince $n0 'hold_tab idx=0 held=true active=0 stopped=\d+ kept=\d+'
  "  trace  : $($h -join ' / ')"
  Check (($h -join '') -match 'stopped=1 kept=0') "the real idle screen was judged safe to stop (stopped=1 kept=0)"
  Start-Sleep -Seconds 3
  $a = Tree $cc
  "  after : $($a.Count) processes under ccnest, claude.exe x$($a.Claude), $($a.MB) MB private (freed $($b.MB - $a.MB) MB)"
  Check ($a.Claude -eq 0) "claude.exe is gone"
  # The pseudo console host of the pane stays (about 1 MB) until ccnest closes the pane.
  Check ($a.Count -le 1 -and $a.MB -lt 20) "at most the pty host is left under ccnest ($($a.Count) process, $($a.MB) MB)"
  Check ($s.Bottom.Contains($FREED)) "the status bar says the memory was freed"
  Check ($s.Frame.Contains($ON_HOLD)) "the frame says it is on hold"

  "4. Enter"
  $n0 = TraceLen
  [void](Send $cc 'enter')
  Check ((TraceSince $n0 'wake_pane id=\d+ woke=true').Count -eq 1) "wake_pane ... woke=true"
  $t = Wait-Status { param($t) $t.Contains('BANANA') -and $t -match 'status=NeedsAttention' } $WaitSec
  Start-Sleep -Seconds 4
  $t = StatusText
  "  the screen after waking (last lines):"
  Show-Tail $t 9
  $w = Tree $cc
  "  awake : claude.exe x$($w.Claude) ($($w.Resume) with --resume $($sid.Substring(0, 8))...), $($w.MB) MB private"
  Check ($w.Resume -ge 1) "claude was started again with --resume <the same session id>"
  Check ($t.Contains('BANANA') -and $t -match 'status=NeedsAttention') "the conversation is back on the screen"

  "5. release and hold again: a resumed session is stopped as well"
  [void](Send $cc 'alt:c')
  $n0 = TraceLen
  [void](Send $cc 'alt:c')
  $h = TraceSince $n0 'hold_tab idx=0 held=true active=0 stopped=\d+ kept=\d+'
  "  trace  : $($h -join ' / ')"
  Check (($h -join '') -match 'stopped=1 kept=0') "stopped=1 kept=0"
  Start-Sleep -Seconds 3
  Check ((Tree $cc).Claude -eq 0) "claude.exe is gone again"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc }
  if ($p) { Kill-Tree $p.Id }
  Start-Sleep -Milliseconds 800
  Remove-Item $logDir -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
