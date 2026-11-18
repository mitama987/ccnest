# Fake claude for idle-compaction-waiting.ps1 (ASCII only: PowerShell 5.1 reads a
# BOM-less script as ANSI). Draws fixtures/idle-compaction-step1.txt, waits for the
# flag file, then draws step2. `{RULE}` becomes a rule of the pane width - 1.
param([string]$Dir, [string]$Flag)
[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false
$rule = ([string][char]0x2500) * ([Console]::WindowWidth - 1)
function Show($name) {
  $t = [IO.File]::ReadAllText((Join-Path $Dir $name), [Text.Encoding]::UTF8)
  $t = $t.Replace("`r", '').Replace('{RULE}', $rule).TrimEnd("`n")
  [Console]::Clear()
  [Console]::Write($t.Replace("`n", "`r`n"))
}
Show 'idle-compaction-step1.txt'
while (-not (Test-Path $Flag)) { Start-Sleep -Milliseconds 300 }
Show 'idle-compaction-step2.txt'
Start-Sleep -Seconds 600
