# url-click-wrap.ps1 の偽 claude。ペイン幅を見て、長い URL を右端で割った形を 2 通り出して待つ。
#  A: Claude Code が自分で割る形 (右端を 1 桁空けて改行し、続きを字下げ 2 桁で置く)
#  B: 最終列まで埋めて端末の自動折り返しに任せ、字下げを空白で書く形
[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false
$url = 'https://docs.google.com/document/d/1QZlfxaJs8FVooN-kZVKeb8lnJBc86sHAPZk9dFEMe_A/edit'
$w = [Console]::WindowWidth
# URL の 1 行目を約 40 文字にするため、前置きを全角文字で伸ばす。"  " + 全角 k 個 + " (" = 4 + 2k 桁
$k = [Math]::Max(1, [int][Math]::Floor(($w - 44) / 2))
$prefix = '  ' + ('原' * $k) + ' ('
$p = 4 + 2 * $k
$out = [Console]::Out
$out.Write("`r`n")
# A
$headA = $url.Substring(0, $w - 1 - $p)
$out.Write($prefix + $headA + "`r`n" + '  ' + $url.Substring($headA.Length) + ")・リンクは開ける`r`n`r`n")
# B
$headB = $url.Substring(0, $w - $p)
$out.Write($prefix + $headB + '  ' + $url.Substring($headB.Length) + ")・リンクは開ける`r`n")
$out.Flush()
Start-Sleep -Seconds 600
