$content = Get-Content 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs' -Raw

$old = @"
// If arguments empty, still send one empty arguments chunk? Spec says chunk2 has arguments.
                  // Already covered by head chunk arguments:""; send nothing extra for empty.
              }
              let _ = send_sse(tx, &ctx.chunk(serde_json::json!({}), Some("tool_calls"))).await;
"@

$new = @"
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
              let _ = send_sse(tx, &ctx.chunk(serde_json::json!({}), Some("tool_calls"))).await;
"@

$content = $content -replace [regex]::Escape($old), $new

Set-Content 'C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs' -Value $content -Encoding utf8
Write-Host "Done"