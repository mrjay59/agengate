p='src/provider.rs'
s=open(p).read()
old='''        // Direct path check
        if std::path::Path::new(bin).exists() {
            return true;
        }

        // PATH search via which crate
        if which::which(bin).is_ok() {
            return true;
        }

        // Windows: try common extensions
        if cfg!(target_os = "windows") {
            for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                if !bin.to_lowercase().ends_with(ext) {
                    let candidate = format!("{}{}", bin, ext);
                    if which::which(&candidate).is_ok() {
                        return true;
                    }
                }
            }
        }

        false
    }'''
new='''        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("--version");
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        match tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(out)) => {
                tracing::info!(provider = %self.id, bin = %bin, status = %out.status, "check_available: resolved");
                true
            }
            Ok(Err(e)) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    tracing::warn!(provider = %self.id, bin = %bin, "check_available: NOT found on PATH");
                    false
                } else {
                    tracing::warn!(provider = %self.id, bin = %bin, error = %e, "check_available: spawn error, assuming present");
                    true
                }
            }
            Err(_) => {
                tracing::warn!(provider = %self.id, bin = %bin, "check_available: timed out");
                false
            }
        }
    }'''
idx=s.find(old)
assert idx!=-1, "not found"
s=s[:idx]+new+s[idx+len(old):]
open(p,'w').write(s)
print("done")
