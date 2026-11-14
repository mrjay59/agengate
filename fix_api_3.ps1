$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs'
$content = Get-Content $path -Raw

# Fix 3: Cancelled kill/tree+cleanup in notifications/cancelled handler
$old = @'
        "notifications/cancelled" => {
            return (axum::http::StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response();
        }
'@

$new = @'
        "notifications/cancelled" => {
            // Cancelled: kill CLI child process, cleanup temp dir, stop run.
            let _ = registry.bridge.cancel_session(&session_id).await;
            return (axum::http::StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response();
        }
'@

$content = $content -replace [regex]::Escape($old), $new

Set-Content $path -Value $content -Encoding utf8
Write-Host "Fix 3 done"