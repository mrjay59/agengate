$path = "C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs"
$content = Get-Content $path -Raw

# Fix empty args handling - replace the exact comment lines
# First replace the comment
$content = $content -replace "// If arguments empty, still send one empty arguments chunk\? Spec says chunk2 has arguments.", "// If arguments empty, spec requires one empty arguments chunk (chunk 2 with empty args)."

# Then replace the second comment line and add code after it
$content = $content -replace "// Already covered by head chunk arguments:\"\"; send nothing extra for empty.", "if args.is_empty() {\n                      let tc = serde_json::json!({\n                          `\"index`": index,\n                          `\"id`": call.id,\n                          `\"type`": call.r#type,\n                          `\"function`": {\n                              `\"name`": call.function.name.as_deref().unwrap_or(`""`),\n                              `\"arguments`": `""`\n                          }\n                      });\n                      send_delta(tx, ctx, serde_json::json!({ `\"tool_calls`": [tc] })).await;\n                  }"

Set-Content $path -Value $content -Encoding utf8
Write-Host "Fixed empty args handling"