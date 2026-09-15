# Continuous integration and releases

Releases go to GitHub Releases only. Nothing is sent to Apple, and nothing is committed, tagged
or pushed unless you run `scripts/release.sh` and answer yes.

| Workflow | When it runs | What it does |
|---|---|---|
| **CI** (`.github/workflows/ci.yml`) | Every push to `main` and every pull request | Checks the C header, runs clippy and every core test, then builds the app and runs the action, content-watcher, helper and panel smoke tests. The built zip is kept with the run for 7 days. |
| **Release** (`.github/workflows/release.yml`) | A pushed `vX.Y.Z` tag | Checks that the tag matches the Makefile `VERSION` and is on `main`, runs the tests, builds and signs the app, and **publishes** the release with the zip, the cheatsheet and SHA-256 checksums. |

Both use GitHub's `macos-26` Apple silicon runners with Xcode 26.6 and Rust 1.95.0, like this Mac.
No setup is required.

## Release a version

```sh
scripts/release.sh
```

That's all. The script:

1. **Picks the version.** If the Makefile `VERSION` has no tag yet it uses that (so the first run
   releases 0.2.8); otherwise it bumps the patch number. You can also pass `minor`, `major` or an
   exact version such as `1.0.0`.
2. **Adds release notes when there are none.** It writes a `**X.Y.Z changes**` list into
   `docs/new-features.md` from your commit messages since the last release, and offers to open it
   for editing.
3. **Runs the checks CI runs:** header, clippy, tests and action tests.
4. **Asks once**, then commits `Release X.Y.Z`, tags `vX.Y.Z` and pushes both.
5. **Follows the GitHub run** and prints the link to the published release.

Options:

- `--yes`: no questions.
- `--no-checks`: skip step 3.
- `--no-watch`: stop after pushing.

If a check fails, fix it and run the script again: its own edits to the Makefile and guide are kept.

## Try a build on this Mac first

```sh
make install
```

This builds and signs the app, replaces `~/Applications/Blindspot.app`, and relaunches it. Only one
copy is kept on disk.

## Opening a downloaded release

The app is not notarized, so macOS blocks the first launch of a downloaded copy. The release notes
tell people to open **System Settings → Privacy & Security** and click **Open Anyway**, once, or to run
`xattr -dr com.apple.quarantine /Applications/Blindspot.app`. Builds you make with `make install`
are not affected.

## Optional: keep permissions across updates

By default the Release workflow signs ad hoc. That works, but the signature changes with every
build, so after each update macOS asks again for Accessibility and other permissions. To sign
releases with your Developer ID instead:

1. In Keychain Access, select **Developer ID Application: seif boukerdenna (VZR89A8Z89)** together with
   its private key, choose **File → Export Items…**, and save `DeveloperID.p12` with a password.
2. Add it to GitHub, then delete the file:

   ```sh
   base64 -i DeveloperID.p12 | gh secret set MACOS_CERTIFICATE_P12_BASE64 --repo SeifBoukerdenna/blindspot
   gh secret set MACOS_CERTIFICATE_PASSWORD --repo SeifBoukerdenna/blindspot
   rm DeveloperID.p12
   ```

The next release picks it up automatically. This uses only your certificate and does not contact Apple.

## If something fails

| Problem | Fix |
|---|---|
| The release run failed | Run `gh run view --log-failed` to see why. Fix and push to `main`, then delete the tag (`git push --delete origin vX.Y.Z && git tag -d vX.Y.Z`) and run `scripts/release.sh X.Y.Z` again. |
| Tag does not match VERSION | Only happens with tags made by hand; `scripts/release.sh` always keeps them in step. |
| A release for the tag already exists | Delete it on the Releases page, then re-run the workflow from the Actions tab. |
