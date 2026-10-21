# SPDX-License-Identifier: GPL-3.0-or-later
# Writes the GPL text (LICENSE) as RTF for the installer's license page.
param([string]$Out = "License.rtf")
$text = Get-Content -Raw -Path LICENSE
$text = $text.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
$text = $text -replace "`r?`n", "\par`r`n"
$rtf = "{\rtf1\ansi\deff0{\fonttbl{\f0\fmodern Consolas;}}\f0\fs16`r`n" + $text + "}"
Set-Content -Path $Out -Value $rtf -Encoding ascii
