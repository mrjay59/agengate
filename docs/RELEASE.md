# AgentGate Release Checklist

Simple checklist for publishing a tagged GitHub release.

## Prerequisites

- Clean working tree on the intended release branch (currently `recover`).
- GitHub Actions enabled for `mrjay59/agengate`.
- `gh` CLI authenticated if you want to verify/create releases manually.

## 1. Bump version

1. Update `Cargo.toml` `version = "X.Y.Z"` (example: `0.3.43`).
2. Run:

   ```powershell
   cargo check
   cargo test --lib
   ```

3. Optional but recommended: run the safe E2E subset used by CI.

## 2. Commit

```powershell
git add Cargo.toml Cargo.lock docs/RELEASE.md .github/workflows/build.yml
git commit -m "vX.Y.Z: <short summary>"
```

Include any other release-related files you intentionally changed.

## 3. Tag and push

```powershell
git push origin recover
git tag vX.Y.Z
git push origin vX.Y.Z
```

Notes:
- `.github/workflows/build.yml` triggers on pushes to `main`/`recover` and on any `v*` tag.
- The `release` job runs only for `refs/tags/v*`, so tags work regardless of the default branch.

## 4. Verify GitHub Actions

1. Open the Actions run for the tag.
2. Confirm these jobs succeed: lint/check, Windows/Linux/macOS builds, verify-all-artifacts, release.
3. Confirm release assets exist:

   - `agentgate-windows-x86_64.zip`
   - `agentgate-windows-aarch64.zip`
   - `agentgate-linux-x86_64.zip`
   - `agentgate-linux-aarch64.zip`
   - `agentgate-macos-x86_64.zip`
   - `agentgate-macos-aarch64.zip`
   - `SHA256SUMS.txt`

4. Verify checksums locally after download:

   ```bash
   sha256sum -c SHA256SUMS.txt
   ```

## 5. Deploy

- **Local:** replace the installed `agentgate` binary, restart service/process, check `/_health`.
- **Nodes:** deploy the matching OS/arch asset, restart `agentgate`, verify `/_health` and a smoke `/v1/models` or chat request.

## 6. Post-release

- Record release tag and commit SHA in the ops/task note.
- If the release contains operational fixes, confirm logs/metrics on nodes after restart.
- Do not deploy from dirty/uncommitted working tree state.
