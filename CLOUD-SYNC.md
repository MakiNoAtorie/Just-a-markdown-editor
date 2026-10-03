# Cloud sync: design proposal

Status: proposal, nothing built yet. You chose **both OneDrive and Nextcloud (selectable)**; your Nextcloud runs over **Tailscale**, is always on and is backed up. The answers so far are in section 1, OneDrive details are in sections 4 and 7, and the Nextcloud option and a comparison are in section 7b. Still to verify on your server: Notify Push and Tailscale HTTPS (section 12, items 2 and 3).

## 1. What we decided

| Question | Answer | What it means for the design |
|---|---|---|
| Where do notes sync to? | **Both selectable**: **OneDrive** through its existing sync, and your own **Nextcloud** over Tailscale (see 7b). No server of our own to build. | The editor stays a plain-files editor. On desktop, either provider just looks like a folder. The provider becomes a per-workspace setting; on the phone it picks the `Storage` backend (section 7b). |
| Which devices? | Windows, Mac, Android phone and iOS. | The Rust core must be reusable outside this desktop UI, and the file layer must cope with phone file-access rules. |
| Conflicts? | Automatic text merge. | We need a three-way merge that works on prose, not just on lines. |
| Encryption? | Optional end-to-end. | An opt-in layer that encrypts before the provider sees the file. Off by default. |
| Local copy? | **Every device keeps the full set of files downloaded.** | No cloud-only placeholders by requirement: set OneDrive to "Always keep on this device", Nextcloud to full sync (no virtual files), and the phone app to a full local cache. Search can read everything, and the editor works offline. Placeholder handling (section 4 item 3, section 7) shrinks to a safety check, not a feature. |
| Speed? | As close to real time as possible. | We need to be honest about what we control and what we don't (section 3). |

## 2. Where we are today

The groundwork from the spec (section 14) is already in the code, so this is an extension, not a rewrite.

- **One storage boundary.** Every read, write, list, rename, delete and watch goes through the `Storage` trait in `src/core/storage.rs`. `LocalStorage` is the only implementation. Paths are workspace-relative everywhere.
- **Atomic saves.** `write` writes a temp file in the same folder, flushes it, then replaces the target. It retries briefly if antivirus or another reader holds the file.
- **Change detection.** `notify` watches the workspace folder recursively. On an event we re-read the file and compare it with the text we last saved (`Document::saved_snapshot`). Identical content is ignored.
- **External edits.** If the document has no unsaved edits it reloads silently. If it has, the user sees the "Keep my edits / Load disk version" banner. `write_checked` repeats that comparison at save time, so we never overwrite a changed file without noticing.
- **Undo is an operation log**, and the session file is kept apart from settings.
- **`core/` has no UI dependencies** (buffer, markdown, commands, workspace, config). That is what makes an iOS or Android port plausible.

So today you can already open a folder inside your OneDrive and it mostly works. The gaps are in how we behave around a sync client, and in what happens when two devices edit the same note.

## 3. How close to real time can this be?

With a provider in the middle, the path of one edit looks like this:

```
keystroke
  -> our autosave pause (700 ms today)
  -> atomic write to disk
  -> provider notices and uploads            (provider's speed)
  -> provider downloads on the other device  (provider's speed)
  -> our file watcher fires and we debounce  (ours)
  -> read, merge, redraw                     (ours)
```

We control the first and last two steps. We do not control the middle. Rough, unmeasured expectations:

- OneDrive typically uploads a saved file within seconds and pushes the change to your other devices within seconds. Treat "a few seconds" as the expectation, not a guarantee.
- OneDrive pauses syncing in some situations (battery saver, metered connections, the user pausing it, being signed out). Neither we nor any app can see that state through a public API, so "synced" can never be promised, only "changed on another device N seconds ago".
- On iOS the app is only really "live" while it is open in the foreground (section 7).
- **Measure first.** Before building anything for speed, write a tiny probe that stamps a note on one device and logs when it appears on another. That gives real numbers for your PCs and phone (see phase 0).

So a realistic goal with this approach is **"a few seconds while both devices are open"**, not sub-second. If you later decide sub-second matters, that needs a channel of our own (section 11, phase 7). I would measure real latencies on your actual setup before deciding it is worth the extra cost.

What we can tune: shorten the autosave pause for synced workspaces (for example 700 ms to about 300 ms), debounce watcher events for about 150-300 ms instead of reacting to each one, and make the merge step fast.

## 4. Phase 0: be a good citizen next to a sync client

These are all small, and they remove most of the "weird" behaviour people see when an editor shares a folder with OneDrive.

1. **Temp file names.** `tempfile` creates a `.tmpXXXX` file next to the note on every save, and OneDrive may briefly try to upload it. Use a recognisable name such as `.<note>.md.jame-tmp` and test on your machine whether OneDrive skips it (names starting with `~$` and `desktop.ini` are documented as skipped; I have not verified `.tmp`-style names, so this needs an experiment). Make the pattern configurable. OneDrive copes with the save-then-rename pattern, which Office uses too, but this should also be tested while a sync is in progress.
2. **Partial writes.** A provider may write a file in pieces. After a watcher event, wait until size and modified time stop changing for a short moment (the debounce above) before reading. A truncated read should retry rather than reload.
3. **Files On-Demand placeholders.** With OneDrive's Files On-Demand, notes you have not opened on this PC can exist only as stubs, and reading one forces a download. The workspace-wide search currently reads every note, which would download the whole folder. Detect placeholders (Windows marks them with the recall-on-data-access / offline file attributes), skip them in search, dim them in the tree, and download only when the user opens one. The simplest fix on your side is to right-click the notes folder in File Explorer and choose **Always keep on this device**; the editor can detect that setting (the pinned attribute) and show a one-line tip when it is not set.
4. **OneDrive conflict copies.** When two devices collide anyway, OneDrive keeps both by creating a second file that, as far as I know, has the other computer's name added (like `note-DESKTOP-ABC123.md`). Confirm the exact naming on your accounts, then recognise it, group the copy under the original note in the tree, and offer "Merge with the other version" instead of leaving a mystery file.
5. **Sync-aware defaults.** Detect that the workspace is inside the OneDrive folder (Windows sets `OneDrive`, `OneDriveConsumer` and `OneDriveCommercial` environment variables) and use the shorter autosave pause automatically.
   - **Latency probe.** A small developer command that writes a timestamp into a scratch note and, on another machine, logs how long the change took to arrive. Run it before and after tuning, so "near real time" is a measured number.
6. **Status line.** Show "Changed on another device 4 s ago" and "Merged changes from another device" in the status bar. We cannot know the provider's upload state, so we say only what we can observe.

## 5. Phase 1: automatic merge

This is the core of the feature and the most valuable part.

### 5.1 Inputs

When a file changes on disk while the user has unsaved edits, we have everything needed for a three-way merge:

- **base**: the text we last saved or loaded (`saved_snapshot`)
- **ours**: the current in-editor text
- **theirs**: the new text on disk

### 5.2 Why line-based merging is not enough here

Markdown paragraphs are usually one long line. A line-based merge (like plain `git merge`) would call two edits to different sentences of the same paragraph a conflict. So:

- Split text into tokens (words and whitespace, with punctuation kept separate) and run the three-way diff over tokens. Edits in different sentences of one paragraph then merge cleanly.
- For very large files (above roughly 1 MB, to be tuned) fall back to line-level, so merging never blocks the interface. Run the merge on a background thread like the other whole-file work.
- Crates such as `similar` (diffing) are enough to build this; the merge rule on top is small. A dedicated merge crate may also work and is worth a quick evaluation.

### 5.3 When both sides changed the same spot

Never lose text. Policy, in order:

1. If the edits don't overlap, merge silently.
2. If they overlap, keep **our** text in the editor, save **theirs** as a conflict copy next to the note (`note (other device, 2026-10-03 14-22).md`), and show a banner: "Both devices edited this part. Your version is kept. Compare / Use theirs / Dismiss". A simple side-by-side compare view is a good small follow-up.
3. Never write conflict markers (`<<<<<<<`) into notes; they look broken in a note-taking app.

### 5.4 Integration with the editor

- Apply the merge as **one undoable edit** in the existing operation log, so Ctrl+Z reverts a merge you don't like.
- Move the cursor and selection through the incoming changes so the caret doesn't jump.
- After merging, save the merged text. Two devices merging the same inputs must produce the same output (a fixed tie-break, for example by device id), otherwise they can ping-pong rewrites forever. This needs a test that runs two instances against one folder.
- The existing path stays as the fallback: if merging fails or times out, show the current "Keep my edits / Load disk version" banner.

### 5.5 Testing

Property tests: merging with an unchanged side returns the other side; merging is deterministic; no input text is ever dropped (every token from ours or theirs is either present or in the conflict copy). Plus a two-instance harness over a temp folder that simulates delays, partial writes and provider conflict copies.

## 6. Phase 2: storage layer changes needed for other platforms

The trait is good but too desktop-shaped. Before porting, evolve it:

- Add a **version token** to reads and writes (modified time plus size and a hash, or an ETag). `write_if(path, text, expected_version)` replaces the "re-read and compare everything" approach and works for any backend.
- Make operations **async or callback-based** so a platform can coordinate file access (below).
- Report **change events with a reason**, so we can tell our own writes from external ones.
- Split the crate: `core` (portable, no UI, no OS APIs), `desktop` (the current winit app), and a small `ffi` crate (for example with UniFFI) exposing parse, search, merge and storage to Swift and Kotlin.
- Move device-specific paths out of the executable folder. Right now settings and session live next to the `.exe`, which suits a portable app but not a synced workspace. Keep **settings and keymap portable** (and optionally synced) and **session and window state per device**, as the spec already says.

## 7. iOS (and Android) realities

This is where "existing folder-sync service" gets harder, so it is worth knowing early.

- **Folder access is by permission, not by path.** The user picks a folder through the system file picker, and the app keeps a security-scoped bookmark to reopen it. There is no free recursive file watching like on desktop.
- **File access must be coordinated.** For cloud folders, reads and writes go through `NSFileCoordinator`; changes can be observed with `NSFilePresenter` (and `NSMetadataQuery` for iCloud). iCloud Drive is the best-supported provider. Other providers (OneDrive, Dropbox, Nextcloud) appear through the Files app via File Provider extensions; coordinated access works, but change notifications to other apps are best-effort. Plan to also re-scan when the app comes to the foreground and poll every few seconds while it is open.
- **No background sync of our own.** iOS suspends apps. "Real time" on iOS means the app is in the foreground; otherwise the provider's own app does whatever it is allowed to do, and we catch up when we are opened.
- **Placeholders are normal.** Files may not be downloaded yet. We need a "downloading..." state and must never treat a stub as an empty note.
- **Android** is similar in spirit: the Storage Access Framework gives folder access by permission, and watching is limited.
- **The UI cannot be this one.** The custom CPU-drawn editor is built around desktop input. A phone editor needs native text input, selection handles, dictation and accessibility. Recommended split: a native shell (SwiftUI/UIKit and Kotlin) with the shared Rust core behind the FFI crate for markdown parsing, search and merge. Drawing the whole editor from Rust on iOS is possible with winit, but I would not recommend it for a text editor.
- **macOS** is easier but modern providers there also use File Provider, so placeholders and coordination apply too.

### OneDrive on the phone: two ways to do it

**Option A: through the Files app (File Provider).** The OneDrive iPhone app shows your OneDrive inside the system Files picker. Our app opens the folder with the standard picker, keeps access with a bookmark, and reads and writes through coordinated file access. Least code, and it reuses the same "it is just a folder" model as desktop. The downsides: the OneDrive app must be installed and signed in, update notifications to our app are best-effort, and files may be placeholders until downloaded. Mitigate with a re-scan when the app comes to the foreground and a short polling loop while it is open.

**Option B: talk to OneDrive directly through the Microsoft Graph API.** We sign in with Microsoft's authentication library and use the REST API: list changes since the last check (a "delta" query), download changed notes, and upload edits with an `If-Match` ETag so a concurrent change is rejected instead of overwritten. That ETag check maps neatly onto the version token proposed in section 6. Polling the delta every few seconds while the app is open could give tighter and more predictable latency than option A. The costs are real: an app registration with Microsoft, OAuth sign-in, our own local cache and upload queue (in effect a small sync client), handling rate limits, and differences between personal and business accounts (check, for example, where delta queries are allowed). Push-style change notifications (webhooks) need a public server, so they are out unless we later add a relay.

**Recommendation:** build the desktop features first, then prototype option A on a real phone and measure it. Move to option B only if A's latency or reliability is not good enough. If B is needed, implement it as another `Storage` backend so the editor and merge code do not change, and keep desktop on plain folders either way. Android would follow the same split, using the system file picker for A or the same Graph backend for B.

## 7b. Option: your Nextcloud over Tailscale

Because the storage layer is a trait, "where notes live" can be a plug-in. A `WebDavStorage` backend would talk to Nextcloud directly, with no desktop sync client in between. Nextcloud exposes your files over WebDAV, an open protocol, which is why this is considerably less work than the OneDrive direct option.

What makes it attractive:

- **Simple, well-understood API.** Listing, reading and uploading are ordinary web requests. Each file has an ETag (a version tag), and an upload can say "only write if it is still this version", which gives safe concurrent-edit detection and maps directly onto the version token proposed in section 6. Sign-in can use an app password, so there is no app registration and no OAuth library.
- **Cheap change detection.** Nextcloud updates the ETag of every parent folder when something inside changes (check this on your version). So one tiny request for the top folder's ETag tells us whether anything changed, and only then do we walk down to the changed files. That makes polling every 2-3 seconds affordable even without push.
- **Optional push.** If your server has the Notify Push app, connected apps can be told about changes immediately instead of polling. I have not verified that the push service is usable by a third-party app in your setup, so this needs a test. Even without it, the polling above is decent.
- **You control the server.** No sync pausing, no cloud-only placeholders, no vendor rules, and you can tune the speed.
- **Tailscale helps a lot.** Nothing is exposed to the public internet, and Tailscale clients exist for Windows, Mac, Android and iOS. Tailscale can also provide HTTPS certificates for your machine names, which matters on iOS: it refuses plain `http://` by default, and a self-signed certificate is awkward there. Use the Tailscale-provided HTTPS name and the phone treats it as a normal secure site.

What to plan for:

- **The phone must be on the tailnet.** Tailscale on iOS is a VPN profile; if it is off or iOS stops it, Nextcloud is unreachable. The editor must therefore be **local-first**: every note is cached on the device, editing never waits for the network, and changes upload when the server is reachable again. We want that regardless of provider.
- **Latency over Tailscale** is low when the devices connect directly and higher if traffic goes through Tailscale's relay. Usually fine for text.
- **You run the server:** updates, backups, disk space, and the machine hosting it staying on. A note app that depends on it needs you to trust its uptime. Nextcloud's own file-version feature (if enabled) is a useful safety net alongside the editor's own merge safety.
- **Desktop with a sync client vs direct WebDAV.** Nextcloud's desktop client works like OneDrive's (a folder, checked on an interval unless push is on). Using it means desktop needs nothing new. Using `WebDavStorage` on desktop too would give faster change detection but means we maintain a local cache and upload queue ourselves. Suggested: keep desktop on the folder, use `WebDavStorage` first on the phone.
- **Local cache is the real work in both "direct" options:** a folder of cached notes, a record of each note's last-known server version (the merge base from section 5), and an upload queue. This part is shared with the OneDrive direct option.

### OneDrive and Nextcloud side by side

| | OneDrive | Nextcloud over Tailscale |
|---|---|---|
| Desktop | A folder; works now with the Phase 0 fixes | A folder via the Nextcloud client; works now with the same fixes |
| iPhone, simplest route | Files app via the OneDrive app (option A), best-effort updates | Files app via the Nextcloud app, same limits |
| iPhone, direct route | Microsoft Graph: app registration, sign-in library, rate limits, account-type quirks | WebDAV: app password, simple requests, ETags |
| Speed | A few seconds, and out of our control | Under our control; push possible; polling is cheap |
| Cloud-only placeholders | Yes (Files On-Demand) | None with direct WebDAV |
| Server to run | None | Yes, you run it |
| Works away from home | Always | Needs Tailscale connected |
| Risk | Provider pauses and changes | Your own uptime and backups |
| Encryption | Optional layer from section 8 | Same optional layer |

**Suggestion:** do the desktop work (phases 0-2) first because it helps with both. Then prototype the phone with whichever route is cheapest to try. Given Tailscale, I would try `WebDavStorage` against your Nextcloud for the phone first, since it is the smaller job and gives you the most control over speed. Keep OneDrive as the zero-maintenance option for the desktop folder, or as a later direct backend if you want it on the phone too. Both are now confirmed as choices: the provider is a per-workspace setting, and a workspace uses exactly one provider at a time (no syncing the same notes through both, which would just create conflicts between the two providers).

The practical consequence: build the desktop sync features (phases 0-1) first, since they are useful on their own and define the merge and storage code the phone apps will reuse.

## 8. Optional end-to-end encryption

Keep it a per-workspace opt-in, implemented as a **decorator around `Storage`** (an `EncryptedStorage` that wraps another storage), which fits the existing boundary cleanly.

- Encrypt each note with an authenticated cipher (for example XChaCha20-Poly1305) using a key derived from a passphrase (Argon2id). Keep the key in the OS secure store (Windows DPAPI/Credential Manager, Keychain on Apple platforms) rather than on disk.
- File format: a small header (magic, version, salt, nonce) followed by ciphertext.
- Merge still works: the app holds the key, so it can decrypt ours, theirs and base, merge in plaintext, and re-encrypt. Provider conflict copies are ciphertext too, but we can decrypt and merge those as well.
- **Costs to be upfront about:** other apps and the provider's web preview can't read the notes; server-side search is gone (we index locally); this deliberately breaks the spec rule that plain markdown files are the only source of truth, so it must stay clearly opt-in per workspace; and losing the passphrase means losing the notes.
- File names and folder structure still leak to the provider unless we also encrypt names, which hurts browsing in the provider's own app. Suggested first version: encrypt contents only and say so clearly in the settings text.

## 9. What stays the same

- Notes are plain `.md` files in an ordinary folder; you can always open them with anything else (except encrypted workspaces).
- Atomic saves, the undo operation log, and the "keep which version" flow stay. Merge is added in front of the banner, not instead of it.
- No account, no server, no telemetry.

## 10. Risks

| Risk | Mitigation |
|---|---|
| OneDrive is slower than you hope, or paused | Measure with the probe first; make the status line honest ("changed on another device N seconds ago"); only then consider phase 7. |
| Notes are cloud-only placeholders on one PC | Detect and show them, skip them in search, tip to set "Always keep on this device". |
| Merge produces a surprising result | Single undoable edit, conflict copy when overlapping, and the old banner as fallback. |
| Two devices rewrite each other forever | Deterministic merge with tie-break, plus a two-instance test. |
| Placeholder or partial-file reads corrupt a note | Debounce, size/time stability check, never treat a stub as content, UTF-8 validation (already present). |
| OneDrive quirks (temp files, conflict naming, placeholders) | Configurable patterns; test with OneDrive first (personal and business if both apply). |
| iOS is more constrained than expected | Prototype folder access and change observation on a device before committing to the iOS plan. |
| Encryption lock-in or key loss | Opt-in, clear warnings, export/decrypt command, recovery explained up front. |

## 11. Suggested order of work

| Phase | What | Rough size | Why this order |
|---|---|---|---|
| 0 | Latency probe, then sync-friendly behaviour for OneDrive on desktop (section 4) | 2-3 days | Gives real numbers first; immediate benefit and low risk. |
| 1 | Merge engine, undo integration, conflict copies (section 5) | about a week | The feature people notice. Pure, testable core code. |
| 2 | Latency tuning and status line | 1-2 days | Makes "near real time" feel real. |
| 3 | Storage trait evolution and crate split (section 6) | about a week | Prerequisite for any phone app. |
| 3b | `WebDavStorage` for Nextcloud, with local cache and upload queue (section 7b) | 1-2 weeks | The smallest route to a real phone backend; reusable for any WebDAV server. |
| 4 | Prototype on a real phone and measure: Nextcloud over Tailscale first, and the OneDrive Files-app route (option A) for comparison | days | De-risks the phone plan before committing; decides whether OneDrive's Graph API (option B) is ever needed. |
| 5 | iOS app on the shared core | weeks | Scope depends on the questions below. |
| 6 | Optional end-to-end encryption | about a week | Independent; can happen any time after phase 3. |
| 7 | Only if needed: our own fast channel | weeks | A tiny relay that tells other devices to re-scan immediately, or carries the edits themselves for sub-second sync. This adds a server and a lot of complexity, so only if measured latency is not good enough. |

Phases 0-2 are worth doing even if the phone app never happens.

## 12. Questions still open

Answered so far: provider (**both selectable**: OneDrive and Nextcloud over Tailscale), devices (all), merging (automatic), encryption (optional), speed (as close to real time as possible), Nextcloud host (always on, backed up).

To verify (you think so / not known yet):

- **Notify Push installed?** **Yes, mostly working.** Your Nextcloud is the All-in-One (AIO) Docker setup, which ships a `nextcloud-aio-notify-push` container, and it is running. `docker exec -u www-data nextcloud-aio-nextcloud php occ notify_push:self-test` (run with `sudo`) passed redis, database and connection checks, and failed only the "trusted proxy" check: the client address arrives as `127.0.0.1` because a proxy in the chain (probably `172.18.0.7`) drops the `X-Forwarded-For` header. That check exists so the push server can pass the real client IP to Nextcloud (used for brute-force protection and logs), not for delivering notifications, and AIO's own configuration already trusts `172.18.0.0/16`. So I expect push to work and treat the warning as cosmetic until the test below says otherwise. To confirm, open `https://<your-host>/ocs/v2.php/cloud/capabilities?format=json` while signed in and look for a `notify_push` entry with endpoints; the real proof is a later phase-4 test from the phone. Original ways to check, for reference:
  - In Nextcloud as admin: profile menu, **Apps**, search "Client Push" (app id `notify_push`). If it is listed as installed or enabled, the app is there.
  - Or on the server with `occ` (a bare `occ` is usually not on the PATH; it lives in the Nextcloud folder and runs as the web user, e.g. `sudo -u www-data php occ ...`, `docker exec -u www-data <container> php occ ...` or `nextcloud.occ ...` on snap): `occ app:list | grep notify_push`, then `occ notify_push:self-test`. The self-test is the real check, because the app also needs its small push service running and your reverse proxy forwarding `/push/` to it; the app alone is not enough.
  - Or ask the server: sign in and open `https://<your-host>/ocs/v2.php/cloud/capabilities?format=json`. If the JSON contains a `notify_push` entry with endpoints, push is set up.
  - If it is missing, we start with polling (section 7b) and add push later as an upgrade.
- **Tailscale HTTPS working?** Open the Nextcloud address in your phone's browser. If it starts with `https://`, the name ends in `.ts.net`, and there is no certificate warning, it is set up. On the server, `tailscale cert <name>.ts.net` issues the certificate, and the name must also be in Nextcloud's `trusted_domains`.

Still open:

5. **Personal or business/school OneDrive account?** It matters for the OneDrive direct option.
6. **What is the latency target in numbers?** "Under 3 seconds", "under 10", "I just want it to feel live"? The phase 0 probe will tell us what each provider achieves.
7. **What should the phone app do?** Quick capture and reading, or a full editor with all formatting?
8. ~~Always keep on this device~~ **Decided:** every instance holds the full files. The editor only warns if it finds a placeholder.
9. **Should settings sync between your devices?** Theme, fonts and shortcuts could follow you, or stay per device.
10. **For encryption, is hiding note contents enough, or must file names be hidden too?**
11. **How many devices write at the same time in practice?** Mostly one at a time, or often two at once? This decides how much to invest in merging versus avoiding conflicts.
12. **Is a small visible `.jame` folder inside the workspace acceptable** for sync metadata (device id, last merge state)?
