$ErrorActionPreference = 'SilentlyContinue'
$base = "C:\Users\sucot\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\png-0.17.16\src\decoder"
Write-Output "=== files in decoder/ ==="
Get-ChildItem $base | Select-Object -ExpandProperty Name

Write-Output "=== stream.rs: text chunk handling ==="
$s = "$base\stream.rs"
Select-String -Path $s -Pattern "tEXt|zTXt|iTXt|fn handle_chunk|uncompressed_latin1|utf8_text|compressed_latin1|ChunkType|fn next_chunk" | ForEach-Object { $_.LineNumber.ToString() + ": " + $_.Line.Trim() }

Write-Output "=== stream.rs handle_chunk body ==="
$c = Get-Content $s
$ln = (Select-String -Path $s -Pattern "fn handle_chunk" | Select-Object -First 1).LineNumber
if ($ln) { $c[($ln-1)..([Math]::Min($ln+60, $c.Length-1))] }
