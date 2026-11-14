$body = Get-Content -Path test_local.json -Raw
$response = Invoke-WebRequest -Uri http://127.0.0.1:8787/v1/chat/completions -Method POST -Headers @{"Content-Type"="application/json"; "Authorization"="Bearer default_key"} -Body $body -TimeoutSec 30
Write-Output $response.Content