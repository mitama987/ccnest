# Fake claude for unseen-turn-end.ps1 (ASCII only: PowerShell 5.1 reads a BOM-less
# script as ANSI). Tab N (counted in <Work>\n.txt) is named alpha / bravo / cc, prints 80
# history lines and a busy line ("esc to interrupt" = Busy for ccnest), then waits for
# <Work>\done<N>. Then it appends a finished reply (60 lines, so the busy line leaves the
# screen) and a Claude Code style input box. The footer under the box has the background
# pill "1 shell" when <Work>\bg<N> exists (= Background for ccnest), else no pill
# (= NeedsAttention). Every line carries "tab<N>" so the test can tell the panes apart in
# CCNEST_STATUS_TRACE. The screen is never cleared (clearing would drop the history the
# test scrolls into).
param([string]$Work)
[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false
$counter = Join-Path $Work 'n.txt'
$n = 1
if (Test-Path $counter) { $n = [int](Get-Content $counter) + 1 }
Set-Content $counter $n
$names = @{ 1 = 'alpha'; 2 = 'bravo'; 3 = 'cc' }
$name = $names[$n]
if (-not $name) { $name = "other$n" }
[Console]::Title = $name

$sb = New-Object Text.StringBuilder
foreach ($i in 1..80) { [void]$sb.Append("tab$n history line $i`r`n") }
[void]$sb.Append("* Working... (esc to interrupt) tab$n")
[Console]::Write($sb.ToString())

$flag = Join-Path $Work "done$n"
while (-not (Test-Path $flag)) { Start-Sleep -Milliseconds 300 }

$rule = ([string][char]0x2500) * ([Console]::WindowWidth - 1)
$prompt = [string][char]0x276F
$dot = [string][char]0x00B7
$footer = "  >> bypass permissions on $dot tab$n"
if (Test-Path (Join-Path $Work "bg$n")) { $footer = "  >> bypass permissions on $dot 1 shell $dot tab$n" }
$sb = New-Object Text.StringBuilder
[void]$sb.Append("`r`n")
foreach ($i in 1..60) { [void]$sb.Append("tab$n reply line $i`r`n") }
[void]$sb.Append("* Worked for 9s $dot 1 shell still running`r`n`r`n")
[void]$sb.Append("$rule`r`n$prompt `r`n$rule`r`n$footer")
[Console]::Write($sb.ToString())
Start-Sleep -Seconds 600
