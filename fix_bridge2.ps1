$path = 'C:\Users\EDN\Documents\AION\project\agentgate\src\bridge.rs'
$content = Get-Content $path -Raw

# Fix cancel_session to use provider_sessions (trait with close) instead of BridgeSession struct
$old = @'
    /// Cancel a session: kill CLI child process, cleanup workdir, stop run.
    pub async fn cancel_session(&self, session_id: &str) -> Result<()> {
        let session = self.get_session(session_id).await;
        if let Some(session) = session {
            session.close().await?;
        }
        self.remove_session(session_id).await;
        Ok(())
    }
'@

$new = @'
    /// Cancel a session: kill CLI child process, cleanup workdir, stop run.
    pub async fn cancel_session(&self, session_id: &str) -> Result<()> {
        // Close provider session (has close() on BridgeSession trait)
        if let Some(provider_session) = self.provider_sessions.write().await.remove(session_id) {
            provider_session.close().await?;
        }
        // Remove bridge session (struct)
        self.remove_session(session_id).await;
        Ok(())
    }
'@

$content = $content -replace [regex]::Escape($old), $new

Set-Content $path -Value $content -Encoding utf8
Write-Host "Fixed cancel_session"