$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\bridge.rs'
$content = Get-Content $path -Raw

$old = @'
    pub async fn remove_session(&self, session_id: &str) -> bool {
        let removed = self.sessions.write().await.remove(session_id).is_some();
        // Dropping the senders wakes any held `tools/call` with "channel closed".
        self.pending_calls
            .lock()
            .await
            .retain(|_, pending| pending.session_id != session_id);
        removed
    }

    pub fn validate_token
'@

$new = @'
    pub async fn remove_session(&self, session_id: &str) -> bool {
        let removed = self.sessions.write().await.remove(session_id).is_some();
        // Dropping the senders wakes any held `tools/call` with "channel closed".
        self.pending_calls
            .lock()
            .await
            .retain(|_, pending| pending.session_id != session_id);
        removed
    }

    /// Cancel a session: kill CLI child process, cleanup workdir, stop run.
    pub async fn cancel_session(&self, session_id: &str) -> Result<()> {
        let session = self.get_session(session_id).await;
        if let Some(session) = session {
            session.close().await?;
        }
        self.remove_session(session_id).await;
        Ok(())
    }

    pub fn validate_token
'@

$content = $content -replace [regex]::Escape($old), $new

Set-Content $path -Value $content -Encoding utf8
Write-Host "Added cancel_session to BridgeServer"