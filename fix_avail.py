p='src/provider.rs'
s=open(p).read()
anchor='// Direct path check (handles absolute/relative paths)'
i=s.find(anchor)
assert i!=-1
# find the end: next "false\n    }" after i
end=s.find('false\n    }', i)
assert end!=-1
end += len('false\n    }')
replacement='''        // Use the same resolution logic as scan_models/build_chat_command so
        // availability matches what the rest of the code actually uses.
        if let Some(resolved) = self.resolve_binary(bin) {
            tracing::info!(provider = %self.id, bin = %bin, resolved = %resolved, "check_available: resolved");
            return true;
        }
        tracing::warn!(provider = %self.id, bin = %bin, "check_available: NOT found on PATH");
        false
    }'''
s=s[:i]+replacement+s[end:]
open(p,'w').write(s)
print("done, replaced at", i)
