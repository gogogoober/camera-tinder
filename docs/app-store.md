# Mac App Store

How Darkroom gets onto the Mac App Store, and what is still missing.

## What's in the repo

| File | What it does |
| --- | --- |
| `src-tauri/tauri.conf.json` | Every build: category Photography, macOS 14+ (EventKit full access needs it), copyright. |
| `src-tauri/Info.plist` | Calendar usage strings; `ITSAppUsesNonExemptEncryption = false` (xxh3 is a hash, not encryption). |
| `src-tauri/tauri.appstore.conf.json` | Merged on top for App Store builds only: entitlements, provisioning profile, privacy manifest, `.app` only. |
| `src-tauri/appstore/entitlements.plist` | App Sandbox, user-selected folders (read-write), app-scoped bookmarks, calendars. No network. |
| `src-tauri/appstore/PrivacyInfo.xcprivacy` | No tracking, no data collected; file timestamps (scan) and disk space (offload) with their reasons. |
| `scripts/build-appstore.sh` | Builds, signs, wraps in a signed `.pkg`. `LOCAL_CHECK=1` builds an ad-hoc signed bundle to inspect. |

Normal builds (`npm start`, `tauri build`) are **not** sandboxed: the sandbox is only in the App Store config, because
the app can't reopen saved folders inside the sandbox yet (see below).

`KeepAwake` during an offload is an `NSProcessInfo` activity (`cull_platform_apple::power`), not `caffeinate`, so it
works in the sandbox.

## One-time setup (Apple's side)

1. Apple Developer Program membership.
2. Certificates (Xcode → Settings → Accounts → Manage Certificates, or developer.apple.com):
   **Apple Distribution** (signs the app) and **Mac Installer Distribution** (signs the `.pkg`; shows in the keychain
   as "3rd Party Mac Developer Installer").
3. An explicit App ID `app.darkroom.desktop` (Identifiers). No extra capabilities are needed.
4. A **Mac App Store** provisioning profile for that App ID → save it as `src-tauri/appstore/embedded.provisionprofile`
   (gitignored).
5. App Store Connect → new macOS app with that bundle id. Needs: a name (see below), privacy policy URL, App Privacy
   = "Data Not Collected", age rating, category Photography, screenshots (e.g. 2880×1800), description, price.

## Build and upload

```sh
rustup target add x86_64-apple-darwin          # once, for the universal build
APPLE_TEAM_ID=ABCDE12345 scripts/build-appstore.sh
```

Then upload `target/universal-apple-darwin/release/bundle/Darkroom-<build>.pkg` with the Transporter app.
The build number (`CFBundleVersion`) defaults to `yymmdd.HHMM` so every upload is higher; the version shown in the
store is `version` in `tauri.conf.json`. Apple Silicon only: `TARGET=aarch64-apple-darwin`.

Check a bundle without certificates: `LOCAL_CHECK=1 TARGET=aarch64-apple-darwin scripts/build-appstore.sh`
(prints the entitlements; checked Nov 2026: sandbox entitlements, Info.plist keys and the privacy manifest are in place).

## Still to do before submitting

- [ ] **Security-scoped bookmarks** (blocker). In the sandbox, access to a picked folder ends when the app quits, so
      folders reconnected at launch (`saved_session_sources` in src-tauri) find nothing. Save a bookmark per source
      when it's added, resolve it at launch (`startAccessingSecurityScopedResource`). Same for resuming an offload.
- [ ] **Safety checks must fail closed** in the sandbox: `is_external_and_connected`, `known_source_ids` and the
      export/offload path checks treat an unreadable folder as "not there", which would unlock Clear previews while a
      card is plugged in. Also `$HOME` is the container there, so the "internal disk" list needs the real home.
- [ ] **Run the sandboxed build for real** on a real card: add folder, scan, cull, export, offload, calendar, restart.
- [ ] Data: the sandboxed app uses `~/Library/Containers/app.darkroom.desktop/`; existing data isn't moved (decide
      whether to offer an import).
- [ ] Hide testing tools in release builds (Reset days, error log, Known folders) or accept the review risk.
- [ ] Tighten the CSP (`null` today; needs `img-src` for `preview:`), checked in the real app.
- [ ] **Name**: "Darkroom" is already an App Store photo app; the store name must be unique (trademark risk too).
- [ ] Store listing must not promise RAW support yet.
