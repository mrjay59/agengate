$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs'
$content = Get-Content $path -Raw

# Fix 1: Chunk 1 name default
$content = $content -replace '"name": call\.function\.name,', '"name": call.function.name.as_deref().unwrap_or(""),'

Set-Content $path -Value $content -Encoding utf8
Write-Host "Fix 1 done"