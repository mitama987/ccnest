# v0.1.54: a running background agent is "background" (green) even when the turn ends
# without the "Waiting for 1 background agent to finish" line. Checked with the REAL claude.
#
# A disposable ccnest runs with no console window (CREATE_NO_WINDOW). The real claude is asked
# to start one background agent (it runs `ping -n <SleepSec> 127.0.0.1`) and to end its turn.
# The verdict and the screen text are read from CCNEST_STATUS_TRACE on every change and kept
# in -OutDir (snap-NN-<status>.txt): these are the source of the test strings in status.rs.
#  1. idle prompt                                   -> NeedsAttention
#  2. turn ended, the agent row is below the footer -> Background
#  3. the agent finished and the notified turn ended -> NeedsAttention
# Keys go in through WriteConsoleInputW (inner mode of status-marker.ps1). The user's own
# ccnest is not touched. Uses a light model. ASCII only (PS 5.1 reads a BOM-less script as ANSI).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$OutDir = (Join-Path $env:TEMP 'ccnest-e2e-agent-list-real'),
  [int]$SleepSec = 90,
  [string]$Model = 'sonnet',
  # Start the agent without run_in_background (the harness default decides how it runs).
  [switch]$Default
)
$ErrorActionPreference = 'Stop'

foreach ($k in @([Environment]::GetEnvironmentVariables('Process').Keys)) {
  if (($k -like 'CLAUDE*' -or $k -like 'AI_AGENT*' -or $k -like 'CCNEST_*') -and $k -ne 'CLAUDE_CONFIG_DIR') {
    [Environment]::SetEnvironmentVariable($k, $null, 'Process')
  }
}
$exe = (Resolve-Path $Exe).Path
$typer = Join-Path $PSScriptRoot 'status-marker.ps1'
New-Item -ItemType Directory -Force $OutDir | Out-Null
Get-ChildItem $OutDir -File | Remove-Item -Force
$tag = 'ccnest-agent-' + (Get-Date -Format 'HHmmss')
$work = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\target')).Path $tag
New-Item -ItemType Directory -Force $work | Out-Null
$traceFile = Join-Path $work 'status-trace.txt'
$typeOut = Join-Path $work 'type-out.txt'
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$env:CCNEST_CLAUDE_PERMISSION_MODE = 'off'
$env:CCNEST_STATUS_TRACE = $traceFile
$env:CCNEST_TAB_HOLDS = Join-Path $work 'tab-holds.json'
$env:CCNEST_TAB_NAMES = Join-Path $work 'tab-names.json'
$env:ANTHROPIC_MODEL = $Model

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function Trace { if (Test-Path $traceFile) { try { [IO.File]::ReadAllText($traceFile, [Text.Encoding]::UTF8) } catch { '' } } else { '' } }
function Status($t) { if ($t -match 'detected=(\w+)') { $Matches[1] } else { '' } }
$RULE3 = ([string][char]0x2500) * 3
# Lines below the last rule (the footer and the agent list), like ccnest reads them.
function Footer($t) {
  $lines = @($t -split "`r?`n")
  $last = -1
  for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i].Contains($RULE3)) { $last = $i } }
  if ($last -lt 0) { return @() }
  @($lines[($last + 1)..($lines.Count)] | Where-Object { $_ -and $_.Trim() })
}
$PROMPT = [string][char]0x276F
$SPIN = [char]0x2026 + ' *\((\d+h )?(\d+m )?\d+s( |\))'
$BARE_SPIN = '(?m)^\s*\S \S+' + [char]0x2026 + '\s*$'
$WAIT = 'Waiting for \d+ background agents? to finish'
# A row of the agent list that is running: "<elapsed> . <arrow> <n> tokens" at the end.
$ROW = '(\d+h )?(\d+m )?\d+s \S \S [\d.]+[kKmM]? tokens\s*$'
# Is the waiting line the last line above the input box (how ccnest read it up to v0.1.53)?
function Waiting-Is-Last($t) {
  $lines = @($t -split "`r?`n")
  $idx = -1
  for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i] -match $WAIT) { $idx = $i } }
  if ($idx -lt 0) { return $false }
  for ($i = $idx + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i].Contains($RULE3)) { return $true }
    if ($lines[$i].Trim()) { return $false }
  }
  return $false
}
function Replying($t) { ($t -match $SPIN) -or ($t -match $BARE_SPIN) -or (((Footer $t) -join "`n") -match 'to interrupt') }
function Has-Row($t) { [bool](@(Footer $t | Where-Object { $_ -match $ROW }).Count) }
# The same screen with the ticking numbers taken out (to keep one snapshot per shape).
function Shape($t) { ($t -replace '\d+', '#') }
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
$utf8 = New-Object Text.UTF8Encoding $false
$snap = 0; $lastShape = ''
# Poll the trace until $cond is true (or $sec passed). Every new shape of the screen is saved.
function Watch([scriptblock]$cond, [int]$sec) {
  $deadline = (Get-Date).AddSeconds($sec)
  do {
    $t = Trace
    if ($t) {
      $sh = Shape $t
      if ($sh -ne $script:lastShape) {
        $script:lastShape = $sh; $script:snap++
        $name = 'snap-{0:d2}-{1}.txt' -f $script:snap, (Status $t)
        [IO.File]::WriteAllText((Join-Path $OutDir $name), $t, $utf8)
        Write-Host "    [$(Get-Date -Format 'HH:mm:ss')] $name row=$(Has-Row $t) waitline-last=$(Waiting-Is-Last $t) replying=$([bool](Replying $t))"
      }
      if (& $cond $t) { return $t }
    }
    Start-Sleep -Milliseconds 400
  } while ((Get-Date) -lt $deadline)
  return $null
}
function Type-Line($ccId, $text) {
  Start-Process powershell.exe -WindowStyle Hidden -Wait -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$typer`"", '-Inner', 'type', '-ProcId', $ccId, '-Out', "`"$typeOut`"", '-Text', "`"$text`"")
}

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$p = $null; $cc = $null; $sessionId = $null
try {
  $si = New-Object Diagnostics.ProcessStartInfo 'cmd.exe', "/c `"`"$exe`" `"$work`"`""
  $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.WorkingDirectory = $work
  $p = [Diagnostics.Process]::Start($si)
  $deadline = (Get-Date).AddSeconds(20)
  do { Start-Sleep -Milliseconds 500; $cc = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $beforePids -notcontains $_.Id } | Select-Object -First 1).Id } while (-not $cc -and (Get-Date) -lt $deadline)
  if (-not $cc) { throw 'no new ccnest.exe' }
  "ccnest pid: $cc  (claude: $((& (Join-Path $env:USERPROFILE '.local\bin\claude.exe') --version) 2>$null))"

  "1. idle prompt"
  $t = Watch { param($t) $t.Contains($PROMPT) -and (Footer $t).Count -gt 0 -and -not (Replying $t) } 90
  Check ([bool]$t) 'claude prompt and footer are up'
  Start-Sleep -Seconds 3
  $t = Trace
  Check ((Status $t) -eq 'NeedsAttention') "idle is NeedsAttention: $(Status $t)"
  $cl = Descendants $cc | Where-Object { $_.CommandLine -match '--session-id\s+([0-9a-f-]{36})' } | Select-Object -First 1
  if ($cl -and ($cl.CommandLine -match '--session-id\s+([0-9a-f-]{36})')) { $sessionId = $Matches[1] }

  "2. start one background agent and end the turn"
  $how = if ($Default) { 'Use the Agent tool (do not pass run_in_background) to start one general-purpose agent.' } else { 'Use the Agent tool with run_in_background set to true to start one general-purpose agent.' }
  Type-Line $cc "$how Its only job is to run the shell command ping -n $SleepSec 127.0.0.1 once in the foreground (not in the background), wait for it to end and then answer done. Do not wait for the agent and do not check on it. Then reply with only the word started."
  $t = Watch { param($t) (-not (Replying $t)) -and (Has-Row $t) -and $t.Contains($PROMPT) } 150
  Check ([bool]$t) 'the turn ended and a running agent row is below the footer'
  if ($t) {
    Start-Sleep -Seconds 5
    $t = Trace
    "  footer: " + ((Footer $t) -join ' | ')
    "  waiting line on screen: $([bool]($t -match $WAIT))"
    Check (Has-Row $t) 'the agent row is still there 5 s later'
    Check ((Status $t) -eq 'Background') "detected=Background while only the background agent runs: $(Status $t)"
  }

  "2b. one more short turn while the agent runs (the waiting line is not the last line any more)"
  Type-Line $cc "Reply with only the word mango."
  $t = Watch { param($t) (-not (Replying $t)) -and (Has-Row $t) -and $t -match '(?m)^\S\s*mango\s*$' } 60
  Check ([bool]$t) 'the second turn ended while the agent row is still below the footer'
  if ($t) {
    Start-Sleep -Seconds 3
    $t = Trace
    "  waiting line is the last line above the input box: $(Waiting-Is-Last $t)"
    "  last lines above the input box:"
    @($t -split "`r?`n" | Where-Object { $_.Trim() }) | Select-Object -Last 9 | ForEach-Object { "    | " + $(if ($_.Length -gt 120) { $_.Substring(0, 120) } else { $_ }) }
    if (Has-Row $t) { Check ((Status $t) -eq 'Background') "detected=Background after the second turn, the agent still runs: $(Status $t)" }
    else { "  (the agent finished before the second turn ended; not judged)" }
  }

  "3. the agent finishes, the notified turn ends"
  $settled = { param($t) (-not (Replying $t)) -and (-not (Has-Row $t)) -and $t.Contains($PROMPT) }
  $deadline = (Get-Date).AddSeconds($SleepSec + 240)
  $t = $null
  do {
    $first = Watch $settled 20
    if ($first) { Start-Sleep -Seconds 6; $again = Trace; if (& $settled $again) { $t = $again } }
  } while (-not $t -and (Get-Date) -lt $deadline)
  Check ([bool]$t) 'the agent row is gone and nothing is replying'
  if (-not $t) { $t = Trace }
  [void](Watch { param($x) $true } 2)
  "  footer: " + ((Footer $t) -join ' | ')
  Check ((Status $t) -eq 'NeedsAttention') "detected=NeedsAttention after the agent finished: $(Status $t)"

  "snapshots: $OutDir ($(@(Get-ChildItem $OutDir -File).Count) files)"
  if ($fails -eq 0) { 'PASS' } else { "FAIL ($fails)" }
} finally {
  if ($cc) { Kill-Tree $cc }
  if ($p) { Kill-Tree $p.Id }
  Start-Sleep -Seconds 1
  if ($sessionId) {
    $enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
    $proj = Join-Path $env:USERPROFILE ".claude\projects\$enc"
    Remove-Item $proj -Recurse -Force -ErrorAction SilentlyContinue
  }
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
