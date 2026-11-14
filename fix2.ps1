$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs'
$lines = Get-Content $path

# Find and replace the tool_calls emission block
$newLines = @()
$skipUntilToolCalls = $false
$inToolCallsBlock = $false
$toolCallsDepth = 0

foreach ($line in $lines) {
    if ($line -match 'let tool_calls: Vec<serde_json::Value>') {
        $inToolCallsBlock = $true
        # Add Chunk 1 + Chunk 2+ block instead
        $newLines += '                        let tool_calls_head: Vec<serde_json::Value> = calls'
        $newLines += '                            .iter()'
        $newLines += '                            .enumerate()'
        $newLines += '                            .map(|(index, call)| {'
        $newLines += '                                serde_json::json!({'
        $newLines += '                                    "index": index,'
        $newLines += '                                    "id": call.id,'
        $newLines += '                                    "type": call.r#type,'
        $newLines += '                                    "function": {'
        $newLines += '                                        "name": call.function.name.as_deref().unwrap_or(""),'
        $newLines += '                                        "arguments": ""'
        $newLines += '                                    }'
        $newLines += '                                })'
        $newLines += '                            })'
        $newLines += '                            .collect();'
        $newLines += '                        send_delta(tx, ctx, serde_json::json!({ "tool_calls": tool_calls_head })).await;'
        $newLines += ''
        $newLines += '                        // Chunk 2+: arguments (split into <=512 byte chunks per spec §7.4).'
        $newLines += '                        // Each chunk MUST include id, type, index, and function.name (can be empty) per spec.'
        $newLines += '                        for (index, call) in calls.iter().enumerate() {'
        $newLines += '                            let args = &call.function.arguments;'
        $newLines += '                            let mut pos = 0;'
        $newLines += '                            let bytes = args.as_bytes();'
        $newLines += '                            while pos < bytes.len() {'
        $newLines += '                                let mut end = (pos + 512).min(bytes.len());'
        $newLines += '                                // ensure UTF-8 boundary'
        $newLines += '                                while end < bytes.len() && !args.is_char_boundary(end) {'
        $newLines += '                                    end -= 1;'
        $newLines += '                                }'
        $newLines += '                                if end == pos {'
        $newLines += '                                    end = (pos + 1).min(bytes.len());'
        $newLines += '                                }'
        $newLines += '                                let slice = &args[pos..end];'
        $newLines += '                                let tc = serde_json::json!({'
        $newLines += '                                    "index": index,'
        $newLines += '                                    "id": call.id,'
        $newLines += '                                    "type": call.r#type,'
        $newLines += '                                    "function": {'
        $newLines += '                                        "name": call.function.name.as_deref().unwrap_or(""),'
        $newLines += '                                        "arguments": slice'
        $newLines += '                                    }'
        $newLines += '                                });'
        $newLines += '                                send_delta(tx, ctx, serde_json::json!({ "tool_calls": [tc] })).await;'
        $newLines += '                                pos = end;'
        $newLines += '                            }'
        $newLines += '                            // If arguments empty, spec requires one empty arguments chunk (chunk 2 with empty args).'
        $newLines += '                            if args.is_empty() {'
        $newLines += '                                let tc = serde_json::json!({'
        $newLines += '                                    "index": index,'
        $newLines += '                                    "id": call.id,'
        $newLines += '                                    "type": call.r#type,'
        $newLines += '                                    "function": {'
        $newLines += '                                        "name": call.function.name.as_deref().unwrap_or(""),'
        $newLines += '                                        "arguments": ""'
        $newLines += '                                    }'
        $newLines += '                                });'
        $newLines += '                                send_delta(tx, ctx, serde_json::json!({ "tool_calls": [tc] })).await;'
        $newLines += '                            }'
        $newLines += ''
        $skipUntilToolCalls = $true
        continue
    }
    
    if ($skipUntilToolCalls) {
        # Skip lines until we hit "send_delta(tx, ctx, serde_json::json!({ \"tool_calls\": tool_calls })).await;"
        if ($line -match 'send_delta.*tool_calls.*tool_calls\)\)\.await') {
            $skipUntilToolCalls = $false
        }
        continue
    }
    
    $newLines += $line
}

Set-Content $path -Value $newLines -Encoding utf8
Write-Host "Fix 2 done"