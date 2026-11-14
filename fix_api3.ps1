$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs'
$raw = Get-Content $path
$lines = @($raw) | ForEach-Object { $_ }

# Replace line by line
$newLines = @()
foreach ($line in $lines) {
    if ($line -match 'spec says chunk2') {
        $newLines += '                  // If arguments empty, spec requires one empty arguments chunk (chunk 2 with empty args).'
    } elseif ($line -match 'Already covered by head chunk arguments') {
        $newLines += '                  if args.is_empty() {'
        $newLines += '                      let tc = serde_json::json!({'
        $newLines += '                          "index": index,'
        $newLines += '                          "id": call.id,'
        $newLines += '                          "type": call.r#type,'
        $newLines += '                          "function": {'
        $newLines += '                              "name": call.function.name.as_deref().unwrap_or(""),'
        $newLines += '                              "arguments": ""'
        $newLines += '                          }'
        $newLines += '                      });'
        $newLines += '                      send_delta(tx, ctx, serde_json::json!({ "tool_calls": [tc] })).await;'
        $newLines += '                  }'
    } else {
        $newLines += $line
    }
}

Set-Content $path -Value $newLines -Encoding utf8
Write-Host "Done"