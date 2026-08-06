$ErrorActionPreference = 'SilentlyContinue'
$src = "C:\Users\sucot\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\png-0.17.16\src\decoder\mod.rs"
Write-Output "=== handle_chunk signature ==="
Select-String -Path $src -Pattern "fn handle_chunk" | ForEach-Object { $_.LineNumber.ToString() + ": " + $_.Line.Trim() }

Write-Output "=== read_info / read_until_text / read_until_first_idat ==="
Select-String -Path $src -Pattern "fn read_info|fn read_until|read_until_idat|fn next_frame|fn finish" | ForEach-Object { $_.LineNumber.ToString() + ": " + $_.Line.Trim() }

Write-Output "=== tEXt/zTXt/iTXt handling in decoder ==="
Select-String -Path $src -Pattern "tEXt|zTXt|iTXt|text_chunk|uncompressed_latin1|utf8_text|compressed_latin1" | ForEach-Object { $_.LineNumber.ToString() + ": " + $_.Line.Trim() }

Write-Output "=== around first handle_chunk match (lines 60-100) ==="
$c = Get-Content $src
$ln = (Select-String -Path $src -Pattern "fn handle_chunk" | Select-Object -First 1).LineNumber
if ($ln) { $c[($ln-1)..([Math]::Min($ln+44, $c.Length-1))] }
