$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs'
$content = Get-Content $path -Raw

# Fix error: as_deref doesn't work on String
$content = $content -replace 'call\.function\.name\.as_deref\(\)\.unwrap_or\(""\)', 'call.function.name.clone()'

Set-Content $path -Value $content -Encoding utf8
Write-Host "Fixed as_deref error"

# Fix bridge.rs cancel_session self parameter issue
$path2 = 'C:\Users\EDN\Documents\AION\project\agentgate\src\bridge.rs'
$content2 = Get-Content $path2 -Raw

# The cancel_session might be at top level instead of inside impl block
# Check if it's placed correctly
if ($content2 -match 'pub async fn cancel_session') {
    Write-Host "cancel_session found in bridge.rs"
}

Set-Content $path2 -Value $content2 -Encoding utf8
Write-Host "Bridge check done"