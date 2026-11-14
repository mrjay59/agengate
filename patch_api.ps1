$content = Get-Content -Path "C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs" -Raw

# 1. Replace stream_idle_timeout() with config-based timeout + first_token_timer
$old1 = 'let idle_timeout = stream_idle_timeout();'
$new1 = 'let first_token_timeout = Duration::from_secs(registry.defaults.first_token_timeout_secs);
            let mut first_token_timer = Box::pin(tokio::time::sleep(first_token_timeout));
            // Ongoing idle timeout after first token received
            let idle_timeout = Duration::from_secs(registry.defaults.first_token_timeout_secs.max(60));'
$content = $content -replace [regex]::Escape($old1), $new1

# 2. Update comment about silent provider
$old2 = '// A provider that never produces an event (missing CLI, upstream'
$new2 = '// First-token timeout: fail fast if provider produces no initial output'
$content = $content -replace [regex]::Escape($old2), $new2

# 3. Add first_token_timer branch before ticker.tick()
$old3 = '_ = ticker.tick() => {'
$new3 = '_ = &mut first_token_timer, if !ttft_recorded => {
                        // First token timeout fired before any event received
                        let error_msg = format!(
                            "provider '" + '{}"' + '" did not produce first token within {} seconds",
                            provider_id, first_token_timeout.as_secs()
                        );
                        let chunk = serde_json::json!({
                            "id": ctx.id,
                            "object": "chat.completion.chunk",
                            "created": ctx.created,
                            "model": ctx.model,
                            "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
                            "error": { "message": error_msg, "code": "first_token_timeout" }
                        });
                        let _ = send_sse(&tx, &chunk.to_string()).await;
                        // Emit [DONE] to properly terminate SSE stream
                        let _ = tx
                            .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                            .await;
                        terminated_by_error = true;
                        break;
                    }
                    _ = ticker.tick() => {'
$content = $content -replace [regex]::Escape($old3), $new3

# 4. Replace idle timeout check - only apply after first token
$old4 = 'if last_provider_activity.elapsed() >= idle_timeout {'
$new4 = 'if ttft_recorded && last_provider_activity.elapsed() >= idle_timeout {'
$content = $content -replace [regex]::Escape($old4), $new4

# 5. Replace emit_event error with raw SSE error chunk + [DONE]
$old5_start = 'emit_event('
$old5_middle = 'RunEvent::Error {'
$old5_end = 'terminated_by_error = true;'
$new5 = 'let error_msg = format!(
                                "provider '" + '{}"' + '" produced no output for {}s; stream aborted",
                                provider_id, idle_timeout.as_secs()
                            );
                            let chunk = serde_json::json!({
                                "id": ctx.id,
                                "object": "chat.completion.chunk",
                                "created": ctx.created,
                                "model": ctx.model,
                                "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
                                "error": { "message": error_msg, "code": "provider_timeout" }
                            });
                            let _ = send_sse(&tx, &chunk.to_string()).await;
                            let _ = tx
                                .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                                .await;
                            terminated_by_error = true;'

# Replace from emit_event( ... ) through terminated_by_error = true;
$pattern5 = 'emit_event\s*\(\s*&tx,\s*&ctx,\s*&mut writer,\s*RunEvent::Error\s*\{[^}]+\}\s*,\s*activity_via_reasoning,\s*\)\s*\.await;\s*terminated_by_error = true;'
$content = [regex]::Replace($content, $pattern5, $new5)

Set-Content -Path "C:\Users\EDN\Documents\AION\project\agentgate\src\api.rs" -Value $content
Write-Host "Patch applied"
