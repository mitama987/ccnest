# `--resume-prompt-file` / `--resume-permission-mode` (v0.1.46) with the REAL claude, once:
#  1. make a throwaway session headless (`claude -p`, light model)
#  2. reopen it in a disposable ccnest (conhost) with a prompt file + bypassPermissions
#  3. the conversation log must gain: the prompt as a user message, an assistant reply after it,
#     and "permissionMode":"bypassPermissions"
# Nothing is typed: the prompt reaches claude as its positional argument and claude sends it itself.
# -Work must be a folder claude already trusts (a new folder stops at the trust dialog), e.g. <repo>\target\...
# Uses a light model so the check does not eat the Fable allowance. ASCII only (PS 5.1, no BOM).
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$Work = (Join-Path $PSScriptRoot '..\..\target\e2e-resume-prompt'),
  [string]$PromptFile = '',
  [string]$Model = 'sonnet',
  [int]$WaitSec = 150
)
$ErrorActionPreference = 'Stop'

# Run from inside a Claude Code session, its env would switch transcript saving off in the child.
foreach ($k in @([Environment]::GetEnvironmentVariables('Process').Keys)) {
  if (($k -like 'CLAUDE*' -or $k -like 'AI_AGENT*' -or $k -like 'CCNEST_*') -and $k -ne 'CLAUDE_CONFIG_DIR' -and $k -ne 'CCNEST_CLAUDE_BIN') {
    [Environment]::SetEnvironmentVariable($k, $null, 'Process')
  }
}
$env:CCNEST_JP_TITLE = 'off'
$env:CCNEST_USAGE = 'off'
$env:ANTHROPIC_MODEL = $Model

$exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Work | Out-Null
$work = (Resolve-Path $Work).Path
$claude = Join-Path $env:USERPROFILE '.local\bin\claude.exe'
if (-not (Test-Path $claude)) { $claude = (Get-Command claude).Source }
if (-not $PromptFile) {
  $PromptFile = Join-Path $work 'prompt.txt'
  [IO.File]::WriteAllText($PromptFile, "The PC restarted. There is nothing to continue here: reply with the single word RESUMED and stop.`r`n", (New-Object Text.UTF8Encoding $false))
}
$promptText = (([IO.File]::ReadAllText($PromptFile, [Text.Encoding]::UTF8)).TrimStart([char]0xFEFF) -split '\s+' | Where-Object { $_ }) -join ' '
$needle = $promptText.Substring(0, [Math]::Min(12, $promptText.Length))

function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
$fails = 0
function Check($ok, $msg) { if ($ok) { "  ok   $msg" } else { "  FAIL $msg"; $script:fails++ } }

$sid = [guid]::NewGuid().ToString()
$enc = -join ($work.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
$logDir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
$log = Join-Path $logDir "$sid.jsonl"
$before = @(Get-Process ccnest -ErrorAction SilentlyContinue).Count
$p = $null
try {
  "1. make a throwaway session ($Model, headless)"
  Push-Location $work
  try { $out = & $claude -p --model $Model --session-id $sid "Reply with the single word READY." 2>&1 | Out-String } finally { Pop-Location }
  "  claude -p -> $(($out -replace '\s+', ' ').Trim())"
  Check (Test-Path $log) "the session has a conversation log"
  $linesBefore = @(Get-Content $log -Encoding UTF8).Count

  "2. reopen it in a disposable ccnest with the prompt file, in bypass mode"
  $argLine = "`"$exe`" `"$work`" --resume $sid=resume-e2e --resume-prompt-file `"$PromptFile`" --resume-permission-mode bypassPermissions"
  $p = Start-Process -FilePath "conhost.exe" -ArgumentList $argLine -WorkingDirectory $work -PassThru
  $sentAt = $null; $replied = $false
  $until = (Get-Date).AddSeconds($WaitSec)
  while ((Get-Date) -lt $until) {
    Start-Sleep -Seconds 3
    $new = @(Get-Content $log -Encoding UTF8 | Select-Object -Skip $linesBefore)
    $iUser = -1
    for ($i = 0; $i -lt $new.Count; $i++) { if ($new[$i] -match '"type":"user"' -and $new[$i].Contains($needle)) { $iUser = $i; break } }
    if ($iUser -ge 0 -and -not $sentAt) { $sentAt = Get-Date; "  prompt appeared in the log after $([int]((Get-Date) - $p.StartTime).TotalSeconds) s" }
    if ($iUser -ge 0) {
      $after = @($new | Select-Object -Skip ($iUser + 1) | Where-Object { $_ -match '"type":"assistant"' -and $_ -match '"type":"text"' })
      if ($after.Count -gt 0) { $replied = $true; break }
    }
  }
  $new = @(Get-Content $log -Encoding UTF8 | Select-Object -Skip $linesBefore)
  Check ([bool]$sentAt) "the prompt was sent as a user message without anyone typing"
  Check $replied "claude answered it"
  $modes = @($new | ForEach-Object { if ($_ -match '"permissionMode":"([A-Za-z]+)"') { $Matches[1] } })
  "  permission modes logged after the resume: $(($modes | Select-Object -Unique) -join ', ')"
  Check ($modes.Count -gt 0 -and $modes[-1] -eq 'bypassPermissions') "the session runs in bypassPermissions"
  $reply = @($new | Where-Object { $_ -match '"type":"assistant"' -and $_ -match '"type":"text"' } | Select-Object -Last 1)
  if ($reply) { $txt = if ($reply[0] -match '"type":"text","text":"((?:[^"\\]|\\.){0,200})') { $Matches[1] } else { '' }; [IO.File]::WriteAllText((Join-Path $work 'reply.txt'), $txt, (New-Object Text.UTF8Encoding $false)); "  reply saved -> $(Join-Path $work 'reply.txt')" }
  $shim = @(Get-CimInstance Win32_Process -Filter "Name='claude.exe'" | Where-Object { $_.CommandLine -match $sid } | ForEach-Object { ($_.CommandLine -replace [regex]::Escape($promptText), '<PROMPT>') -replace '^.*claude\.exe"?\s*', '' })
  if ($shim) { "  real claude args: $($shim[0])" }
  if ($fails -eq 0) { "PASS" } else { "FAIL ($fails)" }
} finally {
  if ($p) { Kill-Tree $p.Id }
  Start-Sleep -Milliseconds 800
  Remove-Item $logDir -Recurse -Force -ErrorAction SilentlyContinue
  "ccnest processes before=$before after=$(@(Get-Process ccnest -ErrorAction SilentlyContinue).Count)"
}
