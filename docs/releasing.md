# Continuous integration and releases

GitHub Releases is the publication channel. Builds are not notarized and signing uses
`--timestamp=none`. The user owns commits, pushes, tags, signing secrets and publication.
The release entry point is **`./scripts/release.sh` from the repository root**; there is no
root-level `./release.sh`.

## Prepare and publish

1. Review the working tree, including untracked files. Commit the complete feature and
   documentation changes on `main`, then push `main` to origin. New Swift files, fixtures,
   guides and media must be included; the release script only stages Makefile and release notes.
2. Check [the feature guide's release notes](new-features.md#14-privacy-limitations-and-release-notes).
   The release script reuses a matching `**X.Y.Z changes**` entry; otherwise it drafts one from
   commit subjects and offers to open it for editing. Review generated notes for user-facing accuracy.
3. Run:

   ```sh
   ./scripts/release.sh
   ```

The script fetches tags and checks that local `main` equals `origin/main`. If the Makefile version
is untagged it uses that version; otherwise it selects the next patch. `minor`, `major`, `patch`
or an explicit version such as `1.0.0` can be passed instead. It updates Makefile and the guide's
release header, validates the notes, and runs `make check-header check test test-actions`.
These are local preflight checks, not the entire CI/native verification suite.

After confirmation it commits the version/notes if needed, creates the annotated version tag,
and atomically pushes `main` and the tag. The GitHub Release workflow builds the tagged source
and publishes the app. If `gh` is installed and authenticated, the script follows the run and
prints the release URL. It does **not** rebuild or reinstall your local app.

Options: `--yes` answers the publication confirmation automatically, `--no-checks` skips local
preflight, and `--no-watch` returns after pushing. The default interactive path is recommended.
A failed run keeps its version/notes edits so they can be inspected and retried.

## Workflows and artifacts

| Workflow | Trigger | Verification and output |
|---|---|---|
| [CI](../.github/workflows/ci.yml) | Push to `main`, pull request, or manual dispatch | Tooling lint/fixtures, header/clippy/core/retrieval tests, ad-hoc app build, action/updater/content/semantic/vector checks and panel smoke tests (including onboarding and containers); app ZIP retained for 7 days |
| [Release](../.github/workflows/release.yml) | Push of a `vX.Y.Z` tag | Tag/version/main-ancestry and notes checks, tooling/core/native checks, signing and packaging, then publication of ZIP, cheatsheet and checksums |

Exact runner and compiler pins belong to the workflow files. Both use macOS arm64 runners.
Release uploads:

- `Blindspot-X.Y.Z.zip`, containing `Blindspot-X.Y.Z/Blindspot.app`, `START-HERE.md` and `FEATURE-GUIDE.md`.
- `Blindspot-X.Y.Z-Cheatsheet.md`, the full feature guide as a standalone download.
- `Blindspot-X.Y.Z-SHA256SUMS.txt`, checksums for the ZIP and cheatsheet.

Download both named assets before checking the entire checksum file with
`shasum -a 256 -c Blindspot-X.Y.Z-SHA256SUMS.txt`.

## Verify a local build first

For the complete maintainer delivery workflow with an existing Developer ID installation:

```sh
make agent-deliver PLAN=1
make agent-deliver
```

This performs fresh release checks, signs, packages, verifies a rollback archive, installs,
and checks the installed version/process/signature plus preservation of local state. Reports
and rollback paths are printed under `build/agent/`. It does not publish or choose a new version.
See [agent-workflow.md](agent-workflow.md).

For a personal developer installation, `make install SIGN_ID=-` builds/signs ad hoc and installs
without that full verification/rollback workflow. Use your own Developer ID for a stable signature.

## Downloaded apps and updates

Follow [quick-start.md](quick-start.md) for the non-notarized first launch and optional onboarding.
Use **System Settings → Privacy & Security → Open Anyway** if macOS blocks a trusted release.
New installations choose indexing, clipboard and model setup; upgrades preserve existing choices.

**Settings → About → Updates** uses [shell/Updater.swift](../shell/Updater.swift). Preserve:

- Normal published `vX.Y.Z` releases; drafts and prereleases are ignored.
- The ZIP structure and checksum filenames above.
- Makefile's `RELEASE_REPO`, written into Info.plist; forks set their own repository.
- A writable installation directory, such as `~/Applications`.

## Signing on GitHub

The workflow uses a Developer ID certificate when its secrets are configured; otherwise it
signs ad hoc. Ad-hoc identity changes can prompt for macOS permissions again after an update.
The updater warns about that signing mode. Local delivery requires the existing stable identity.

A maintainer can export their Developer ID Application certificate and private key as a
password-protected P12 from Keychain Access, then configure these repository secrets:

- `MACOS_CERTIFICATE_P12_BASE64`: base64-encoded P12.
- `MACOS_CERTIFICATE_PASSWORD`: the P12 password.

Keep that material outside the repository and remove temporary exports after setup. The workflow
imports it into a temporary keychain and cleans up afterward. Configuring secrets is a user-run
administrative step, not a prerequisite for an ad-hoc fork build. No notarization is performed.

## Recovery

- **Local preflight fails:** fix the cause, review retained version/notes changes and rerun.
- **Atomic push fails:** the local release commit/tag may exist. Inspect them before following
  the script's printed retry command; do not blindly create another tag.
- **GitHub build fails:** inspect `gh run view --log-failed` or the Actions page. Retry a transient
  failed job on the same commit. For a code fix, commit it on `main` and publish a new version;
  do not rewrite an already published release tag.
- **Tag/version mismatch:** compare the tagged Makefile with the tag. The release script keeps
  these aligned; hand-created tags can bypass that check.
- **Local installed build needs rollback:** use the archive recorded by `make agent-deliver`
  and the recovery instructions in [agent-workflow.md](agent-workflow.md).
