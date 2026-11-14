$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs'
$content = Get-Content $path -Raw

# Fix 2: Replace the entire tool_calls emission block with Chunk 1 + Chunk 2+
$old = @'
            let tool_calls: Vec<serde_json::Value> = calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    serde_json::json!({
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": {
                            "name": call.function.name.as_deref().unwrap_or(""),
                            "arguments": call.function.arguments
                        }
                    })
                })
                .collect();
            send_delta(tx, ctx, serde_json::json!({ "tool_calls": tool_calls })).await;
'@

$new = @'
// Spec §7.4: emit TWO chunks per tool call.
// Chunk 1: id + name + arguments:""
            let tool_calls_head: Vec<serde_json::Value> = calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    serde_json::json!({
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": {
                            "name": call.function.name.as_deref().unwrap_or(""),
                            "arguments": ""
                        }
                    })
                })
                .collect();
            send_delta(tx, ctx, serde_json::json!({ "tool_calls": tool_calls_head })).await;

            // Chunk 2+: arguments (split into <=512 byte chunks per spec §7.4).
            // Each chunk MUST include id, type, index, and function.name (can be empty) per spec.
            for (index, call) in calls.iter().enumerate() {
                let args = &call.function.arguments;
                let mut pos = 0;
                let bytes = args.as_bytes();
                while pos < bytes.len() {
                    let mut end = (pos + 512).min(bytes.len());
                    // ensure UTF-8 boundary
                    while end < bytes.len() && !args.is_char_boundary(end) {
                        end -= 1;
                    }
                    if end == pos {
                        end = (pos + 1).min(bytes.len());
                    }
                    let slice = &args[pos..end];
                    let tc = serde_json::json!({
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": {
                            "name": call.function.name.as_deref().unwrap_or(""),
                            "arguments": slice
                        }
                    });
                    send_delta(tx, ctx, serde_json::json!({ "tool_calls": [tc] })).await;
                    pos = end;
                }
                // If arguments empty, spec requires one empty arguments chunk (chunk 2 with empty args).
                if args.is_empty() {
                    let tc = serde_json::json!({
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": {
                            "name": call.function.name.as_deref().unwrap_or(""),
                            "arguments": ""
                        }
                    });
                    send_delta(tx, ctx, serde_json::json!({ "tool_calls": [tc] })).await;
                }
            }
'@

$content = $content -replace [regex]::Escape($old), $new

Set-Content $path -Value $content -Encoding utf8
Write-Host "Fix 2 done"