# `--resume-prompt-file` / `--resume-permission-mode` (v0.1.46): real-process check.
# Starts a disposable ccnest in conhost with a fake claude (a cmd that just waits) and reads the
# command line each pane was started with:
#  1. session A (prompt file + bypassPermissions) -> `--resume A --dangerously-skip-permissions "<prompt>"`,
#     the prompt arriving as ONE argument (spaces, a semicolon and Japanese inside), folded to one line
#  2. session B (plain --resume) -> the window default (`--permission-mode plan --allow-...`), no prompt
#  3. misuse exits with an error before any window opens (option before --resume / missing file)
# Does not touch the user's ccnest (no key injection; reads process command lines only).
# ASCII only on purpose (PS 5.1 reads a BOM-less file as ANSI); the Japanese is built from code points.
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe')
)
$ErrorActionPreference = 'Stop'

Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }
Remove-Item env:CCNEST_PID -ErrorAction SilentlyContinue
Remove-Item env:CCNEST_CLAUDE_PERMISSION_MODE -ErrorAction SilentlyContinue
Remove-Item env:CCNEST_CLAUDE_NO_SKIP_PERMISSIONS -ErrorAction SilentlyContinue
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "ccnest-e2e-resume-prompt"
New-Item -ItemType Directory -Force $work | Out-Null
$shim = Join-Path $work 'claude-shim.cmd'
Set-Content $shim @(
  '@echo off',
  'title claude',
  'echo SHIM started',
  'powershell.exe -NoProfile -Command Start-Sleep -Seconds 600'
) -Encoding ASCII
$env:CCNEST_CLAUDE_BIN = $shim

# "tsuzuki" in Japanese + ASCII with spaces and a semicolon, over two lines with a BOM (the file is folded to one line).
$jp = -join ([char]0x7D9A, [char]0x304D)
$promptFile = Join-Path $work 'prompt.txt'
[IO.File]::WriteAllText($promptFile, "$jp go on; ok`r`n  second line`r`n", (New-Object Text.UTF8Encoding $true))
$expected = "$jp go on; ok second line"

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
function PaneLine($rootId, $session) {
  @(Descendants $rootId | Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -match [regex]::Escape($session) } | ForEach-Object { $_.CommandLine })[0]
}
function Wait-Until($sec, $cond) { $until = (Get-Date).AddSeconds($sec); while ((Get-Date) -lt $until) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 500 }; return $false }
function Run($argLine) {
  $o = Join-Path $work 'run.out'; $e = Join-Path $work 'run.err'
  $proc = Start-Process -FilePath $exe -ArgumentList $argLine -NoNewWindow -Wait -PassThru -RedirectStandardOutput $o -RedirectStandardError $e
  $script:last = [pscustomobject]@{ Code = $proc.ExitCode; Err = (Get-Content $e -Raw -Encoding UTF8) }
  "  $ ccnest $argLine"
  "    exit=$($last.Code) | $(("$($last.Err)" -replace '\s+', ' ').Trim())"
}

$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }

$a = [guid]::NewGuid().ToString(); $b = [guid]::NewGuid().ToString()
$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$argLine = "`"$exe`" `"$work`" --resume $a=tab-a --resume-prompt-file `"$promptFile`" --resume-permission-mode bypassPermissions --resume $b=tab-b"
$p = Start-Process -FilePath "conhost.exe" -ArgumentList $argLine -WorkingDirectory $work -PassThru
try {
  Start-Sleep -Seconds 5
  $cc = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1).ProcessId
  if (-not $cc) { throw "no ccnest.exe under conhost $($p.Id)" }
  "disposable ccnest pid: $cc"

  "1. session A: prompt file + bypassPermissions"
  $okA = Wait-Until 12 { [bool](PaneLine $cc $a) }
  Check $okA "the pane for A started"
  $lineA = PaneLine $cc $a
  "  A: $($lineA -replace [regex]::Escape($jp), '<JP>')"
  Check ($lineA -match "--resume\s+$a") "A was started with --resume <id>"
  Check ($lineA -match '--dangerously-skip-permissions') "A starts in bypass mode"
  Check ($lineA -notmatch '--permission-mode') "A does not get the window's plan mode"
  Check ($lineA.Contains('"' + $expected + '"')) "the prompt arrives as one quoted argument, BOM gone, folded to one line"
  Check ($lineA.TrimEnd().EndsWith('"' + $expected + '"')) "the prompt is the last argument"

  "2. session B: plain --resume"
  $okB = Wait-Until 12 { [bool](PaneLine $cc $b) }
  Check $okB "the pane for B started (second tab, 2 s later)"
  $lineB = PaneLine $cc $b
  "  B: $lineB"
  Check ($lineB -match "--resume\s+$b\s+--permission-mode\s+plan\s+--allow-dangerously-skip-permissions\s*`"?\s*$") "B keeps the window default and gets no prompt"

  "3. misuse fails before a window opens"
  Run "--resume-prompt-file `"$promptFile`" --resume $a"
  Check ($last.Code -ne 0 -and $last.Err -match 'must come after a --resume') "option before any --resume"
  Run "--resume $a --resume-prompt-file `"$work\nope.txt`""
  Check ($last.Code -ne 0 -and $last.Err -match 'cannot read') "missing prompt file"
  Run "--resume $a --resume-permission-mode --plan"
  Check ($last.Code -ne 0 -and $last.Err -match 'invalid mode') "mode that is not a word"

  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  Kill-Tree $p.Id
  $inbox = if ($cc) { Join-Path $env:APPDATA "ccnest\requests\$cc" }
  if ($inbox) { Remove-Item $inbox -Recurse -Force -ErrorAction SilentlyContinue }
  Start-Sleep -Milliseconds 500
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
