# Fake claude for hold-sleep.ps1 (ASCII only: PowerShell 5.1 reads a BOM-less script as
# ANSI). Draws a screen shaped like Claude Code's and waits. -Kind:
#   idle  = the reply is done, the input box is empty        -> safe to stop
#   busy  = a spinner line with the elapsed time             -> working, must not be stopped
#   draft = the reply is done, half-written text in the box  -> must not be stopped
# The rule is the pane width - 1; the prompt glyph is U+276F.
param([string]$Kind = 'idle', [string]$Label = '?')
[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false
$rule = ([string][char]0x2500) * ([Console]::WindowWidth - 1)
$prompt = [string][char]0x276F
$dot = [string][char]0x25CF
$lines = switch ($Kind) {
  'busy' {
    @("$prompt do the long thing", '', "* Working$([char]0x2026) (12s $([char]0x00B7) esc to interrupt)", '', $rule, "$prompt ", $rule, '  plan mode on (shift+tab to cycle)')
  }
  'draft' {
    @("$dot SHIM $Label done", '', $rule, "$prompt half written", $rule, '  plan mode on (shift+tab to cycle)')
  }
  default {
    @("$dot SHIM $Label done", '', $rule, "$prompt ", $rule, '  plan mode on (shift+tab to cycle)')
  }
}
[Console]::Clear()
[Console]::Write(($lines -join "`r`n"))
Start-Sleep -Seconds 900
