# 日本語タブ名 (v0.1.25) の実機確認。使い捨て ccnest を conhost で起動し、そのセッション ID の
# 会話ログに英語の ai-title だけの偽ログを置いて、タブ行 (画面 1 行目) をコンソールバッファから読む。
# ユーザーの ccnest には触らない。Haiku を 1 回呼ぶ (約 $0.0002)。
param(
  [string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\ccnest.exe'),
  [string]$AiTitle = "fix-login-redirect-loop"
)
$ErrorActionPreference = 'Stop'
# Claude のセッション内から起動したときの env 漏れを止める
Get-ChildItem env: | Where-Object { ($_.Name -like 'CLAUDE*' -or $_.Name -like 'AI_AGENT*') -and $_.Name -ne 'CLAUDE_CONFIG_DIR' } | ForEach-Object { Remove-Item "env:$($_.Name)" }

$exe = (Resolve-Path $Exe).Path
$cwd = Join-Path $env:TEMP "ccnest-e2e-jp-title"
New-Item -ItemType Directory -Force $cwd | Out-Null
$p = Start-Process -FilePath "conhost.exe" -ArgumentList "`"$exe`"" -WorkingDirectory $cwd -PassThru
function Kill-Tree($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { Kill-Tree $_.ProcessId }; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
function Descendants($id) { Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $id } | ForEach-Object { $_; Descendants $_.ProcessId } }
$fake = $null
try {
  Start-Sleep -Seconds 8
  $claude = Descendants $p.Id | Where-Object { $_.CommandLine -match '--session-id\s+([0-9a-f-]{36})' } | Select-Object -First 1
  if (-not $claude) { throw "no claude with --session-id under conhost $($p.Id)" }
  [void]($claude.CommandLine -match '--session-id\s+([0-9a-f-]{36})'); $sid = $Matches[1]
  $enc = -join ($cwd.ToCharArray() | ForEach-Object { if ($_ -match '[A-Za-z0-9]') { $_ } else { '-' } })
  $dir = Join-Path $env:USERPROFILE ".claude\projects\$enc"
  New-Item -ItemType Directory -Force $dir | Out-Null
  $fake = Join-Path $dir "$sid.jsonl"
  if (Test-Path $fake) { throw "session file already exists: $fake" }
  $lines = @(
    ('{"type":"user","message":{"role":"user","content":"please fix the redirect loop after login"},"sessionId":"' + $sid + '"}'),
    ('{"type":"ai-title","aiTitle":"' + $AiTitle + '","sessionId":"' + $sid + '"}')
  )
  [IO.File]::WriteAllText($fake, ($lines -join "`n") + "`n", (New-Object Text.UTF8Encoding $false))
  "session: $sid  fake: $fake"
  # 10 秒の猶予 + 2 秒 tick + 生成 2〜3 秒 + 2 秒 tick
  Start-Sleep -Seconds 22
  "fake exists after wait: $(Test-Path $fake)  size: $((Get-Item $fake -ErrorAction SilentlyContinue).Length)"
  Get-Content $fake -Encoding UTF8 | ForEach-Object { "  | " + $_.Substring(0, [Math]::Min(160, $_.Length)) }
  "claude procs: " + ((Descendants $p.Id | ForEach-Object { "$($_.Name)" }) -join ', ')
  $c = Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $p.Id -and $_.Name -eq 'ccnest.exe' } | Select-Object -First 1
  $rowFile = Join-Path $env:TEMP "ccnest-e2e-tab-row.txt"
  $reader = Join-Path $PSScriptRoot 'read-console-row.ps1'
  Start-Process powershell.exe -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File',"`"$reader`"",'-ProcId',$c.ProcessId,'-Out',"`"$rowFile`"") -WindowStyle Hidden -Wait
  "tab row (screen line 1):"; Get-Content $rowFile -Encoding UTF8 | Select-Object -First 1
  $cache = Join-Path $env:APPDATA "ccnest\jp-titles.json"
  if (Test-Path $cache) { "cache entry: " + ((Get-Content $cache -Raw -Encoding UTF8 | ConvertFrom-Json).PSObject.Properties | Where-Object { $_.Name -like "$sid*" } | ForEach-Object { $_.Value }) } else { "cache file missing" }
} finally {
  Kill-Tree $p.Id
  if ($fake) { Remove-Item (Split-Path $fake) -Recurse -Force -ErrorAction SilentlyContinue }
}
