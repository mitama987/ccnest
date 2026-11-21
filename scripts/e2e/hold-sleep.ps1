# A tab put on hold (Alt+C) frees its memory: its Claude is stopped when the screen is safe
# to stop, and started again (--resume) by a key (v0.1.49) - check on the real binary.
# Throw-away ccnest with NO console window (CREATE_NO_WINDOW) and a fake claude per tab
# (hold-sleep-shim.ps1 draws a screen shaped like Claude Code's):
#   1 alpha = idle   2 bravo = busy (spinner)   3 cc = draft in the input box
#   4 delta = idle   5 echo  = idle
# A fake session log is placed for every tab, so a woken pane is started with --resume.
# "its Claude is gone / is running" = the shim process (cmd.exe whose command line carries
# the session id) is not / is among ccnest's descendants.
#
#  A. Alt+C on echo (front, idle) -> its Claude is stopped; the status bar says so
#  B. click the held tab -> the last screen is still shown, the frame says "on hold";
#     nothing is started (looking costs no memory)
#  C. Alt+Left / Alt+Right through the sleeping tab -> nothing is started
#  D. Enter in the sleeping pane -> its Claude is started again with --resume <same id>;
#     the tab is still on hold
#  E. Alt+C (release) -> nothing more is started; Alt+C (hold again) -> stopped again;
#     click it + Alt+C (release while asleep) -> started again
#  F. hold the busy tab -> NOT stopped      G. hold the tab with a draft -> NOT stopped
#  H. type a key into an idle tab and hold it at once -> NOT stopped (input was just sent);
#     release, wait, hold again -> stopped
#  I. a second window started with --resume: the session that is on hold comes up without
#     a process; Enter starts it
# -Off: CCNEST_HOLD_SLEEP=off -> holding an idle tab stops nothing.
# ASCII only (no BOM needed): Japanese text is written as code points.
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [switch]$Off
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*' -or $_.Name -like 'CCNEST*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
$exe = (Resolve-Path $Exe).Path
$inner = Join-Path $PSScriptRoot 'dblclick-rename-tab.ps1'
$shimPs = Join-Path $PSScriptRoot 'hold-sleep-shim.ps1'
$tag = 'ccnest-e2e-hold-sleep-' + (Get-Date -Format 'HHmmss')
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
  'set K=idle',
  'if %N%==2 set K=busy',
  'if %N%==3 set K=draft',
  'title %T%',
  "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$shimPs`" -Kind %K% -Label %N%"
) -Encoding ASCII
function Write-Run($path, $tail) {
  $l = @(
    '@echo off', 'set CCNEST_PID=', 'set CCNEST_JP_TITLE=off', 'set CCNEST_USAGE=off', 'set CCNEST_INPUT_TRACE=1',
    "set CCNEST_TAB_NAMES=$(Join-Path $work 'tab-names.json')", "set CCNEST_TAB_HOLDS=$holds", "set CCNEST_CLAUDE_BIN=$shim"
  )
  if ($Off) { $l += 'set CCNEST_HOLD_SLEEP=off' }
  $l += "`"$exe`"$tail"
  Set-Content $path $l -Encoding ASCII
}
$run = Join-Path $work 'run.cmd'
Write-Run $run ''
$screenFile = Join-Path $work 'screen.txt'
$trace = Join-Path $env:APPDATA 'ccnest\input-trace.log'
$DIV = [string][char]0x2502
$DOT = [string][char]0x25CF
function U($codes) { -join ($codes | ForEach-Object { [char]$_ }) }
$ON_HOLD = U 0x4FDD, 0x7559, 0x4E2D                       # "on hold" in the frame title / hint
$FREED = U 0x30E1, 0x30E2, 0x30EA, 0x3092, 0x7A7A, 0x3051  # "freed the memory" in the notice
$KEPT = U 0x52D5, 0x304B, 0x3057, 0x305F, 0x307E, 0x307E   # "left running" in the notice

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id, $all) { $all | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId $all } }
# Session ids of the fake claudes running under ccnest, started with $flag.
function Shims($rootId, $flag) {
  $all = @(Get-CimInstance Win32_Process)
  @(Descendants $rootId $all | Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -match "$flag\s+([0-9a-f-]{36})" } | ForEach-Object { [void]($_.CommandLine -match "$flag\s+([0-9a-f-]{36})"); $Matches[1] })
}
function Running($rootId) { @(Shims $rootId '--session-id') + @(Shims $rootId '--resume') }
function Send($ccId, $ops) {
  if (-not $ops) { $ops = 'sleep:1' }
  Start-Process powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$inner`"", '-Inner', 'run', '-ProcId', $ccId, '-Out', "`"$screenFile`"", '-Ops', "`"$ops`"", '-AlsoRows', '1,2,-1') -WindowStyle Hidden -Wait
  $l = @(Get-Content $screenFile -Encoding UTF8)
  if ($l.Count -lt 5) { throw "inner failed: $($l -join ' ')" }
  @{ Row = $l[0]; Attrs = @($l[1] -split ' '); Text = ($l[0] -replace "`0", '').TrimEnd(); Frame = $l[2]; Body = $l[3]; Status = $l[4] }
}
function Col($s, $needle) { $s.Row.IndexOf($needle) }
function Held($s) {
  $m = [regex]::Matches($s.Row, "$DOT\x00?(\d+) ")
  $cols = @{}
  foreach ($x in $m) { $cols[$x.Groups[1].Value] = $x.Index }
  @{ Nums = (@($m | ForEach-Object { $_.Groups[1].Value }) -join ','); Cols = $cols }
}
function Green($s) { (@([regex]::Matches($s.Row, "[^$DOT]\x00(\d+) ") | ForEach-Object { $_.Groups[1].Value }) -join ',') }
function TraceLen { if (Test-Path $trace) { @(Get-Content $trace -Encoding UTF8).Count } else { 0 } }
function TraceSince($from, $pattern) { @(Get-Content $trace -Encoding UTF8 | Select-Object -Skip $from | Where-Object { $_ -match $pattern } | ForEach-Object { [void]($_ -match $pattern); $Matches[0] }) }
function Wait-For($ccId, $sec, $test) {
  $s = $null
  $deadline = (Get-Date).AddSeconds($sec)
  do { $s = Send $ccId ''; if (& $test $s) { break }; Start-Sleep -Milliseconds 800 } while ((Get-Date) -lt $deadline)
  $s
}
# Wait (up to $sec s) until the fake claude of session $id is / is not running.
function Wait-Running($ccId, $id, $want, $sec) {
  $deadline = (Get-Date).AddSeconds($sec)
  do { $on = (Running $ccId) -contains $id; if ($on -eq $want) { return $true }; Start-Sleep -Milliseconds 500 } while ((Get-Date) -lt $deadline)
  $false
}
# No console window at all (CREATE_NO_WINDOW): nothing shows up and nothing takes the keyboard
# focus from the user. The screen is read and input is sent through the console API.
function New-Ccnest($runFile, $before) {
  $si = New-Object Diagnostics.ProcessStartInfo 'cmd.exe', "/c `"$runFile`""
  $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.WorkingDirectory = $work
  $proc = [Diagnostics.Process]::Start($si)
  $id = $null
  $deadline = (Get-Date).AddSeconds(20)
  do {
    Start-Sleep -Milliseconds 500
    $id = (Get-Process ccnest -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Select-Object -First 1).Id
  } while (-not $id -and (Get-Date) -lt $deadline)
  if (-not $id) { throw "no new ccnest.exe" }
  @{ Proc = $proc; Id = $id }
}
$enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
$logDir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
function New-Log($id) {
  New-Item -ItemType Directory -Force $logDir | Out-Null
  [IO.File]::WriteAllText((Join-Path $logDir "$id.jsonl"), ('{"type":"user","message":{"role":"user","content":"hold sleep test"},"sessionId":"' + $id + '"}' + "`n"), (New-Object Text.UTF8Encoding $false))
}

$beforePids = @(Get-Process ccnest -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$p = $null; $cc = $null; $p2 = $null; $cc2 = $null
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }
try {
  $started = New-Ccnest $run $beforePids
  $p = $started.Proc; $cc = $started.Id
  Start-Sleep -Seconds 5
  $ids = @()
  $ids += @(Running $cc)[0]
  foreach ($i in 1..4) {
    [void](Send $cc 'ctrl:t'); Start-Sleep -Seconds 3
    $ids += @(Running $cc | Where-Object { $ids -notcontains $_ })[0]
  }
  if (@($ids | Where-Object { $_ }).Count -ne 5) { throw "expected 5 fake claudes, got: $($ids -join ', ')" }
  $ids | ForEach-Object { New-Log $_ }
  $s = Wait-For $cc 20 { param($x) (Green $x) -eq '2' }
  "   tab row: " + $s.Text
  $a = Col $s '1:alpha'

  if ($Off) {
    "Off. CCNEST_HOLD_SLEEP=off: Alt+C on echo (idle) stops nothing"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    Check ((TraceSince $n0 'hold_tab idx=4 held=true active=3 stopped=0 kept=0 woken=0').Count -eq 1) "hold_tab ... stopped=0 kept=0"
    Check ((Held $s).Nums -eq '5') "echo is on hold (dot + 5)"
    Start-Sleep -Seconds 2
    Check ((Running $cc) -contains $ids[4]) "its Claude is still running"
    Check (-not $s.Status.Contains($FREED)) "no 'freed the memory' notice"
  } else {
    "A. Alt+C on echo (front, idle)"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    "   status : " + $s.Status
    Check ((TraceSince $n0 'hold_tab idx=4 held=true active=3 stopped=1 kept=0 woken=0').Count -eq 1) "hold_tab idx=4 held=true active=3 stopped=1 kept=0"
    Check ((Held $s).Nums -eq '5') "echo is on hold (dot + 5)"
    Check (Wait-Running $cc $ids[4] $false 6) "its Claude is gone (no process for session $($ids[4].Substring(0, 8)))"
    Check (@(Running $cc).Count -eq 4) "the other four are running (got $(@(Running $cc).Count))"
    Check ($s.Status.Contains($FREED)) "the status bar says the memory was freed"

    "B. click the held tab: look without starting anything"
    $s = Send $cc "click:$((Held $s).Cols['5']),0"
    "   tab row: " + $s.Text
    "   frame  : " + $s.Frame
    "   body   : " + $s.Body
    Check ((Col $s '5:echo') -ge 0) "it shows its name (front tab)"
    Check ($s.Frame.Contains($ON_HOLD)) "the frame says it is on hold"
    Check ($s.Body.Contains('SHIM 5 done')) "the last screen is still shown"
    Check (-not ((Running $cc) -contains $ids[4])) "nothing was started"

    "C. Alt+Left / Alt+Right through the sleeping tab"
    $n0 = TraceLen
    [void](Send $cc 'altleft')
    $s = Send $cc 'altright'
    Check ((Col $s '5:echo') -ge 0 -and $s.Frame.Contains($ON_HOLD)) "back on the sleeping tab"
    Check ((TraceSince $n0 'wake_pane').Count -eq 0 -and -not ((Running $cc) -contains $ids[4])) "nothing was started"

    "D. Enter in the sleeping pane"
    $n0 = TraceLen
    [void](Send $cc 'enter')
    Check ((TraceSince $n0 'wake_pane id=\d+ woke=True|wake_pane id=\d+ woke=true').Count -eq 1) "wake_pane ... woke=true"
    Check (Wait-Running $cc $ids[4] $true 10) "its Claude is running again"
    Check ((Shims $cc '--resume') -contains $ids[4]) "started with --resume <the same session id>"
    Start-Sleep -Seconds 4
    $s = Send $cc ''
    "   tab row: " + $s.Text
    "   frame  : " + $s.Frame
    Check (-not $s.Frame.Contains($ON_HOLD)) "the frame no longer says on hold"
    # The restarted fake claude is the 6th start, so the tab is named other6 now.
    $n5 = [regex]::Match($s.Row, ' 5:').Index
    Check ($n5 -gt 0 -and $s.Row.Substring(0, $n5).TrimEnd(" `0".ToCharArray()).EndsWith($DOT)) "the tab is still on hold (dot)"

    "E. release, hold again, release while asleep"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    Check ((TraceSince $n0 'hold_tab idx=4 held=false active=4 stopped=0 kept=0 woken=0').Count -eq 1) "release: woken=0 (it was awake)"
    Check ((Running $cc) -contains $ids[4]) "still running"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    Check ((TraceSince $n0 'hold_tab idx=4 held=true active=3 stopped=1 kept=0').Count -eq 1) "hold again: stopped=1"
    Check (Wait-Running $cc $ids[4] $false 6) "its Claude is gone again"
    $s = Send $cc "click:$((Held $s).Cols['5']),0"
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    Check ((TraceSince $n0 'hold_tab idx=4 held=false active=4 stopped=0 kept=0 woken=1').Count -eq 1) "release while asleep: woken=1"
    Check (Wait-Running $cc $ids[4] $true 10) "its Claude is running again"
    Check ((Held $s).Nums -eq '' -and $s.Row.IndexOf($DOT) -lt 0) "no tab is on hold"

    "F. hold the busy tab (bravo)"
    # The woken tab gets its name back on the next tick, which shifts the tab bar: read it
    # again once it has settled.
    Start-Sleep -Seconds 5
    $s = Send $cc ''
    $g2 = [regex]::Match($s.Row, "[^$DOT]\x00(2) ").Groups[1].Index
    [void](Send $cc "click:$g2,0")
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    "   status : " + $s.Status
    Check ((TraceSince $n0 'hold_tab idx=1 held=true active=\d+ stopped=0 kept=1').Count -eq 1) "hold_tab idx=1 ... stopped=0 kept=1"
    Start-Sleep -Seconds 2
    Check ((Running $cc) -contains $ids[1]) "its Claude is still running"
    Check ($s.Status.Contains($KEPT)) "the status bar says it was left running"

    "G. hold the tab with a draft in the input box (cc)"
    [void](Send $cc "click:$((Col $s '3:cc') + 3),0")
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    "   tab row: " + $s.Text
    Check ((TraceSince $n0 'hold_tab idx=2 held=true active=\d+ stopped=0 kept=1').Count -eq 1) "hold_tab idx=2 ... stopped=0 kept=1"
    Start-Sleep -Seconds 2
    Check ((Running $cc) -contains $ids[2]) "its Claude is still running"

    "H. a key was just sent to the pane (delta)"
    [void](Send $cc "click:$((Col $s '4:delta') + 3),0")
    $n0 = TraceLen
    $s = Send $cc 'type:x|alt:c'
    "   tab row: " + $s.Text
    Check ((TraceSince $n0 'hold_tab idx=3 held=true active=\d+ stopped=0 kept=1').Count -eq 1) "hold right after a key: stopped=0 kept=1"
    Check ((Running $cc) -contains $ids[3]) "its Claude is still running"
    [void](Send $cc "click:$((Held $s).Cols['4']),0")
    [void](Send $cc 'alt:c')
    Start-Sleep -Seconds 6
    $n0 = TraceLen
    $s = Send $cc 'alt:c'
    Check ((TraceSince $n0 'hold_tab idx=3 held=true active=\d+ stopped=1 kept=0').Count -eq 1) "released, waited 6 s, held again: stopped=1"
    Check (Wait-Running $cc $ids[3] $false 6) "its Claude is gone"

    "I. a second window with --resume: the held session opens without a process"
    $u1 = [guid]::NewGuid().ToString(); $u2 = [guid]::NewGuid().ToString()
    New-Log $u1; New-Log $u2
    $json = Get-Content $holds -Raw -Encoding UTF8 | ConvertFrom-Json
    $json | Add-Member -NotePropertyName $u2 -NotePropertyValue 1
    [IO.File]::WriteAllText($holds, ($json | ConvertTo-Json -Compress), (New-Object Text.UTF8Encoding $false))
    $run2 = Join-Path $work 'run2.cmd'
    Write-Run $run2 " `"$work`" --resume $u1=first --resume $u2=second"
    $started2 = New-Ccnest $run2 (@($beforePids) + $cc)
    $p2 = $started2.Proc; $cc2 = $started2.Id
    $s = Wait-For $cc2 20 { param($x) (Held $x).Nums -eq '2' }
    "   tab row: " + $s.Text
    Start-Sleep -Seconds 3
    $r2 = @(Running $cc2)
    Check ((Held $s).Nums -eq '2') "tab 2 (the held session) is a dot + 2"
    Check ($r2.Count -eq 1 -and $r2[0] -eq $u1) "only tab 1 has a process (running: $($r2.Count))"
    $s = Send $cc2 "click:$((Held $s).Cols['2']),0"
    "   frame  : " + $s.Frame
    "   body   : " + $s.Body
    Check ($s.Frame.Contains($ON_HOLD) -and $s.Body.Contains($ON_HOLD)) "opened: the frame and the screen say it is on hold"
    Check (@(Running $cc2).Count -eq 1) "looking at it starts nothing"
    [void](Send $cc2 'enter')
    Check (Wait-Running $cc2 $u2 $true 10) "Enter starts its Claude"
    Check ((Shims $cc2 '--resume') -contains $u2) "with --resume <its session id>"
  }

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($cc2) { Kill-Tree $cc2 }
  if ($p2) { Kill-Tree $p2.Id }
  if ($cc) { Kill-Tree $cc }
  if ($p) { Kill-Tree $p.Id }
  Start-Sleep -Milliseconds 800
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
  Remove-Item $logDir -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$($beforePids.Count) after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
