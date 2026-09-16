# Port Validation

## Recorded local results

The implementation was checked on Windows x64 with Rust/Cargo 1.98.1 and MSVC:

| Check | Result |
| --- | --- |
| Release EXE and DLL build | Passed; no CRT conflict or PDB filename collision |
| Clippy with `-D warnings` | Passed |
| Rust tests | 13 library tests and 5 launcher tests passed, plus 3 shared-module tests in the example |
| Historical comparison with the original C++ | ABI assertions and hash vectors verified before removing legacy sources |
| DLL API | Six original exports retained; `T7PatchStart` added for the launcher |
| `smoke` example with the Release DLL | Loading, API calls, and unknown-host rejection passed |
| Remote loading into a dedicated test process | DLL loaded, `UNSUPPORTED` reply received, and existing module recognized on a second connection |
| Win32 window | Control creation, message loop, and closing checked; screenshot visually reviewed |
| BO3 on Windows | Pending |
| Linux / Wine / Proton | Pending |

These results describe the implementation checks, not successful in-game validation. Platform setup instructions are in the [README](../README.md#windows-setup).

## Continuous integration

The workflows use GitHub-hosted `windows-2022` runners and stable Rust with `x86_64-pc-windows-msvc`. The runner supplies the Visual Studio tools and Windows SDK needed to build MinHook.

### Automatic checks

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on pushes to any branch and on pull requests. It installs `rustfmt` and Clippy, then runs:

1. `cargo fmt -- --check`.
2. Clippy for all targets with `-D warnings`.
3. All-target tests and a separate documentation-test run.

Superseded check runs for the same branch or pull request are cancelled. Compiling code as part of Clippy and tests does not generate a Release distribution artifact.

### Manual builds

[`.github/workflows/build.yml`](../.github/workflows/build.yml) has only a `workflow_dispatch` trigger. Start it using **Actions > Build > Run workflow** and choose the branch. The workflow must be present on the default branch for GitHub to expose that button.

The manual workflow runs:

1. Release packaging through `scripts/build.ps1` and compilation of the smoke-test examples.
2. The original DLL API smoke test against the packaged DLL.
3. Remote loading and bootstrap replies in the dedicated `launcher_host.exe` process.
4. Update-helper replacement/relaunch check in a temporary installation.
5. Launcher creation and shutdown using `--ui-smoke-test`, with a 30-second process timeout.
6. Artifact upload after all build and smoke-test steps succeed.

The artifact is named `t7patch-windows-x64`, contains the packaged EXE and DLL, and is retained for 14 days. Both workflows use read-only repository permissions and cache Rust dependencies. The manual build runs independently of the automatic check workflow.

The UI lifecycle check does not capture the desktop or start game detection. Screenshot inspection remains a local check. CI does not launch BO3 or validate Wine/Proton, and a successful run does not establish that the in-game patch works.

## Automated checks

### Protection history

The history reader tests cover category separation, observe-only diagnostics, exclusion of raw journal fields from the summary view, malformed counts, partial lines, the 200-entry limit, the 256 KiB read limit, append, truncation and replacement. File reading runs in a dedicated launcher thread; the UI uses its in-memory snapshot.

The four-tab Windows smoke check passed with sample history: all filter states, counters, hidden-control navigation, retained settings edits and clean shutdown. `--ui-smoke-test` uses explicitly labeled sample entries and disables journal reading/opening. The Protection screenshot was reviewed using:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\capture-ui.ps1 -Tab Protection
```

All-target tests (63 passed, 2 manual checks ignored), formatting, Clippy with warnings denied, Release distribution build and DLL loading smoke check passed for this change. New DLL journal entries include UTC timestamps; actual in-game event production and the shortened version label still require an in-game check.

### Updater checks

The updater tests cover stable semantic-version ordering, missing/duplicate assets, invalid manifests, corrupted ZIPs, unexpected ZIP paths, cancellation, Windows x64 binary validation, preserving configuration, restoring the old EXE after a DLL replacement fails, and recovering an interrupted transaction. The locked-DLL regression holds a real Windows file handle that denies deletion. A corrupted backup is rejected before either file is restored. Network requests are disabled in normal tests and in `--ui-smoke-test`.

The optional native WinHTTP check uses the public GitHub endpoint:

```powershell
cargo test --locked --target x86_64-pc-windows-msvc --bin t7patch-launcher github_update_endpoint_smoke -- --ignored --nocapture
```

For an end-to-end helper check, close BO3 and the launcher, build Release and the examples, then run:

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc --examples
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-update.ps1
```

This runs a copy of the real updater helper in a temporary directory under `target`, replaces a fixture EXE/DLL pair and launches the harmless `update_host` successor. The successor only writes a marker and exits; it never loads the patch or searches for BO3. Configuration and installed hashes are verified and the fixture directory is removed. This checks the Windows helper/relaunch path; downloading two published production versions cannot be tested until matching Releases exist.

The tag-triggered [Release workflow](../.github/workflows/release.yml) performs those offline checks and validates the final ZIP/manifest through `--verify-update-package`. Only its publication job has `contents: write`; build and test jobs retain read-only repository access. Version mismatches fail before publication. Drafts are published after both assets upload successfully; re-runs may complete an existing draft but do not replace an already public Release.

### Patch and loader checks

- PE fingerprints for both builds and RVA translation boundaries.
- Sizes, alignment, and offsets of message, session, and Steam structures.
- 32-bit/64-bit hashes and FNV against vectors obtained by compiling the supplied C++.
- CRLF configuration, name limits, empty passwords, and a final line without a newline.
- Saving and replacing configuration at a Unicode path; preserving the previous file when an edit is invalid.
- Startup request layout, bounded replies, and rejection of invalid versions or arguments.
- PE64 export parsing; rejection of invalid tables, forwarded exports, and out-of-range ordinals.
- Malformed localization directives and UI model key limits.
- Join member and heartbeat nominee limits, including negative counts, truncation, and extra elements.
- A real hook on a machine-code function created in the test process: the original returns 7, the hook returns 42, the trampoline returns 7, and disabling the hook restores 7.

Inspectors use a copy of the reader and per-call local storage. Their tests simulate the serialization interface; they do not replace protocol tests against BO3's actual serializers. Unicode fixture values are test data, not application text.

Run the automated checks on Windows:

```powershell
cargo fmt -- --check
cargo clippy --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo test --all-targets --locked --target x86_64-pc-windows-msvc
```

## Reference value provenance

During migration, a helper program compiled the original C++ hashing implementation and structures. Its `static_assert` checks verified the ABI, and its output supplied hash vectors, including non-ASCII bytes.

The legacy sources and helper were removed during project cleanup. The vectors remain fixed in `src/hashing.rs`; size, alignment, and offset checks are in `src/structs.rs`. Both run through `cargo test`. The project no longer includes a C++ comparison command. The original and reference repositories are listed in the [credits](../README.md#credits).

## DLL loading check

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
cargo run --release --locked --target x86_64-pc-windows-msvc --example smoke -- target/x86_64-pc-windows-msvc/release/t7patch.dll
```

This checks `LoadLibrary`, resolution of the six original exports, player names and passwords, null inputs, repeated deactivation, and rejection of installation in a non-BO3 executable.

To test **loading** under standalone Wine, copy `release/examples/smoke.exe` and `release/t7patch.dll` to the Linux machine. Substitute the actual existing test prefix and DLL path:

```sh
WINEPREFIX="/absolute/path/to/test-prefix" \
  wine ./smoke.exe 'Z:\absolute\path\to\t7patch.dll'
```

The Windows DLL path must map to the file inside that prefix. For a Proton environment, use the matching Protontricks setup described in the [Linux instructions](../README.md#linux--steam-deck-setup). This smoke test does not install the Wine exception handler because its host is deliberately unrecognized.

## Launcher loading check

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
cargo build --release --locked --target x86_64-pc-windows-msvc --examples
.\target\x86_64-pc-windows-msvc\release\examples\loader_smoke.exe
```

`loader_smoke` creates its own `launcher_host.exe` process, loads the DLL into it through the actual loading mechanism, and expects an explicit unsupported-executable reply. It connects again to verify recognition of the loaded module. The helper process is terminated when the test finishes. This test does not search for or modify BO3.

## UI check

To check the window with game detection and settings writes disabled:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build.ps1
.\dist\t7patch.exe --ui-smoke-test
```

The window closes automatically after approximately two seconds. To capture an image cropped to that test window on Windows:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\capture-ui.ps1
```

The image is written to `target/launcher-preview.png`. Its dark appearance, blue borders, text fields, friends-only checkbox, link, and status bar were visually reviewed.

UI smoke mode does not demonstrate in-game activation. The launcher protocol distinguishes `WAITING`, `ACTIVE`, `UNSUPPORTED`, `FAILED`, `DEACTIVATED`, and `BAD_REQUEST`. `ACTIVE` is only returned once DLL installation finishes successfully.

## Corrections relative to the original

- `hkLobbyMsgRW_PrepReadMsg` returns an error when reading fails or the prefix does not match; the original ended with `return true` in both cases.
- The instant-message path uses fields from the initialized `msg_t` instead of interpreting packet bytes as a `msg_t`.
- Array bounds reject negative counts, extra elements, and truncated lists with declared sizes. Serialization failures are propagated.
- The `MenuResponseCached` event index is checked before accessing the cache.
- Strings are bounded before copying. `NULL` clears the password, and null names are ignored.
- Hash arithmetic has defined wrapping behavior, including in Debug builds.
- The worker has a stop signal and is joined; installation and deactivation are serialized.
- Original bytes and pointers are saved for restoration, and MinHook/write errors are checked. Failures are reported through `OutputDebugStringA`.
- The DLL remains pinned to preserve callbacks and trampolines that may still be executing. Reinstallation in the same process after deactivation is not supported.
- The launcher waits for required game objects before consuming the installation attempt. Retries during incomplete startup are not treated as permanent failures.
- The EXE and DLL share an absolute configuration path. Closing the window stops its detection worker; the installed DLL retains its own worker and hooks.

These corrections affect protocol cases that the C++ handled inconsistently. Interoperability with another player using the C++ version must be checked, especially when changing the password during a session.

## Control-traffic protection

The `network_guard=control-v2` layer shares the same admission policies in Campaign, Multiplayer and Zombies. It adds no renderer hook or new native game entry point, and preserves the existing Campaign `requeststats` exception. It does not change packet serialization or the network-password format. The original `control-v1` pre-read predicate is no longer enforced at the already-tokenized connectionless command callback; see the startup regression below.

### Admission budgets

These are initial conservative control-traffic budgets, **not limits calibrated from live BO3 traffic**. Each pool has two seconds of burst credit. Byte units below are binary KiB/MiB; social calls have no byte count available.

| Pool | Per peer / second | Global / second | Applied to |
| --- | --- | --- | --- |
| Social | 8 calls | 128 calls | Accept-invite, join-message actions and send-join-info callbacks |
| Instant | 128 messages, 256 KiB | 4096 messages, 8 MiB | Guarded instant-message dispatcher |
| P2P control | 256 messages, 1 MiB | 8192 messages, 32 MiB | P2P type `104` only |
| Connectionless | 128 messages, 256 KiB | 4096 messages, 8 MiB | `infoResponse`, `statusResponse`, `print`, `error`, `ping`, `pinga` |

Other connectionless commands retain the existing allowlist policy. In particular, `connectResponse`, `steamAuthReq`, `keyAuthorize`, `LM`, `fastrestart`, `loadoutResponse`, `statresponse`, `cfl` and Campaign-only stats requests do not use the connectionless budget. This does not bypass any earlier IM/P2P guard through which their transport may pass.

Each pool owns a fixed 256-entry peer table. Idle entries can be reused after 30 seconds; new identities cannot evict active ones and replenish their burst credit. No heap allocation, Steam calls, or disk writes occur while an admission mutex is held. Global budgets charge attempts before the table scan, including attempts subsequently rejected by the per-peer budget. This bounds table work during identity churn, but sustained floods can exhaust the shared pool and temporarily affect legitimate control messages. IP addresses and supplied XUIDs are keys for accounting, not proof of authentication.

### Early message checks

- Instant-message lengths outside 2–2049 bytes are rejected before memory queries or native parsing, matching the existing two-byte header and strictly-less-than-2048-byte payload bound.
- Before the guarded lobby/IM readers run, descriptors reject overflow flags, cursors beyond the combined buffers, sizes outside native signed-length limits, null nonempty buffers, address arithmetic overflow, and bit cursors outside the payload. Both primary and split buffers are checked for readability at those boundaries. Connectionless command dispatch observes this predicate without enforcing it, since its native caller has already consumed command data. These checks require live engine-owned allocations; they do not make concurrent buffer destruction safe.
- Invalid lobby descriptors are marked as rejected before the inspector invokes game readers. Valid message types outside the existing inspection set retain their cursor and payload.
- P2P return sizes are checked against capacity even for packets shorter than the six-byte discriminator header. A rejected successful read clears its reported size.

### Friend-cache retry behavior

The existing Steam-calling thread and refresh eligibility are preserved. Failed refreshes retain the complete previous snapshot and retry after 1, 2, 4, 8, 16, then at most 30 seconds. The delay starts after the failed call completes, and success resets the failure count. Concurrent/reentrant callers still use the previous snapshot. This prevents repeated failed queries from being driven by every inbound social action; it does not move a successful enumeration off-thread or change snapshot invalidation semantics.

### Verification and interpretation

Automated checks cover token refill, byte budgets, burst exhaustion, backward clocks, fixed-table capacity/expiry, concurrent calls sharing a budget, an 18-peer synthetic control stream, connection/Campaign-command exclusions, split-buffer arithmetic, malformed descriptors rejected without native calls, early field-read failures, P2P short/oversized return values, and retry backoff from completion. The structured wire tests are not a substitute for fuzzing BO3's native serializers.

The normal event journal includes `network_guard=control-v2 connectionless_reader=observe-v1 friend_retry=backoff-v1`. Nonzero counters are written in a `network-guard` summary at most every ten seconds by the existing maintenance worker. `*_limited` counts admission rejections; `friends_refresh_failed` counts failed refreshes, not blocked packets. No packet contents, peer identities, addresses or passwords are written by these summaries. A blocked maintenance worker can delay them.

Before calling a mode validated, record clean and malformed-traffic tests in an isolated session: Campaign solo/co-op and checkpoints, MP lobby/join/respawn/map rotation, and Zombies solo/co-op/start/restart/return to menu. Include invitations, reconnects, host migration, matching passwords and the supported executable builds. Compare frame-time distributions in the same scenario before/after this layer and inspect the summary counters for unintended admission limits. Neither live mode compatibility, native-parser fuzz coverage, nor an FPS gain is established by the synthetic tests.

### Startup black screen after control-v1

A local September2026 session with `control-v1` remained at a black screen after loading. Read-only inspection found an active game process and installed DLL, continuing profile reports, approximately 3–4 `envelope` rejections per ten seconds and connectionless calls at the same cadence. Rate-limit and failed-friend-refresh counters stayed at zero. The existing fatal crash log belonged to an older session; no new fatal/suspension event was recorded in this one.

This implicated the newly enforced reader-state check at `CL_ConnectionlessCMD`, which runs after native command tokenization. The v1 journal recorded only an aggregate rejection count, so the precise rejected field was initially unknown.

`control-v2` restores the earlier dispatch behavior at this callback: the command allowlist, Campaign exception and selected control-rate policies decide admission, while the new descriptor predicate is observation-only. The guard still enforces the predicate before the lobby/IM reads it performs itself. It does not reset or repair a game-owned cursor to make it pass validation.

The maintenance worker now writes `reader-diagnostic` samples with stage, `action=observe` or `action=reject`, fault category, a fixed allowlisted command label, and scalar fields (`overflowed`, `capacity`, `cursize`, `split`, `readcount`, `bit`). An unreadable descriptor produces `metadata=unavailable`. The callback uses a nonblocking snapshot slot; there is at most one snapshot per fault category and stage per ten-second report interval (20 slots total). There are no packet bytes, memory addresses, player identifiers or passwords in these samples.

After replacing the binaries and restarting, the user confirmed that the main menu appeared. The new journal captured `stage=connectionless-post-command action=observe reason=Overflowed command=connectResponse overflowed=1 capacity=19 cursize=19 split=0 readcount=19 bit=0`. This identifies the v1 predicate that rejected the bootstrap response: the overflow flag is present at this post-tokenization callback even though the cursor equals the payload length. The fix leaves that native state untouched and forwards the allowlisted command.

A regression test retains those captured scalar values (not the packet contents), alongside synthetic post-command overflow/cursor/capacity cases, original return-value forwarding, unchanged command rejection, Campaign-only stats and rate admission. Startup to the menu is confirmed for this local September2026 run; complete Campaign/MP/Zombies sessions and other executable builds still need the checks above. Restart both processes when replacing the DLL and verify the new session header before interpreting its diagnostics.

## Performance diagnostics

For an FPS regression, first close BO3 and the launcher, run `scripts/build.ps1`, and launch `dist/t7patch.exe`. The script packages **Release** binaries together; plain `cargo build` creates Debug binaries and does not replace the DLL in `dist`. An active DLL with debug assertions enabled now identifies itself in the launcher's status text.

Compare the same menu and map before and after activation, recording FPS/frame times and whether the slowdown persists after `Patch active`. User-supplied September2026 Release profiling showed `memory.bounded_string` consuming nearly the entire sampled `hook.model_path` time (roughly 2.5–4.2 ms per call in several steady-state windows). Removing the duplicate query alone did not resolve the reported 1–2 FPS regression.

The six UI string hooks (`binding`, `model_string`, `model_path0`, `model_path`, `model_create`, `model_alloc`) now use `memory::game_string`: a direct, bounded byte scan with no `VirtualQuery`. This follows the reference's direct `strlen`/copy approach in [`Hooks.cpp`, revision b9450b2](https://github.com/Scroptss/T7Patch-src/blob/b9450b229639193c16ef21effc80aa28745eb683/Hooks.cpp). Model keys must still be shorter than 64 bytes per segment; the port's total path cap of 65536 bytes, 4096-byte localization buffers, null handling, and content sanitizers remain. The reader relies on live, readable engine-owned strings, as the original does; it does not validate arbitrary non-null pointers. It stops at NUL without reading the entire cap or caching memory permissions. Exported inputs and packet-boundary checks retain the queried reader.

The profile header identifies this implementation with **`ui_strings=direct-bounded-v1`**. `memory.game_string` measures the new UI reader, while `memory.bounded_string` measures the remaining queried callers. Subsequent user profiling showed direct UI reads around 0.05–0.1 microseconds and fast model hooks, but also a freeze when entering Zombies. The mode-transition fixes below still require in-game confirmation.

For a local relative-cost check on valid engine-like strings, run:

```powershell
cargo test --release --locked --target x86_64-pc-windows-msvc --lib ui_string_reader_benchmark -- --ignored --nocapture
```

This reports the median of five batches for queried versus direct reads. It is not an in-game benchmark: BO3's virtual-memory layout and query cost differ from the test process. Automated tests cover terminators next to an inaccessible page, length caps, raw byte preservation, and the original per-segment path rules.

If the slowdown persists:

1. Close BO3. Create an empty file named **`t7patch-profile.enabled`** alongside the launcher's `t7patch.conf` (normally in `dist`). For legacy loaders, use the configuration directory instead.
2. Start the launcher and BO3. After activation, reproduce the slow menu or match for about 30 seconds.
3. Read **`t7patch-profile.log`** in that same directory. It appends a session header identifying the game build and Debug/Release assertion setting, then one aggregate report approximately every five seconds. The first window can include installation time; prefer subsequent windows for steady-state comparisons.
4. Share the log and corresponding FPS/frame times. Remove the `.enabled` file and restart BO3 to compare with profiling disabled.

Each row contains `metric calls calls/s samples avg_us max_us`. Hooked game and Steam functions have individual counters; `memory.game_string`, `memory.bounded_string`, `exceptions.handle`, and `exceptions.lobby_sentinel` identify string-reading and exception traffic. Every call is counted, but only the first and then every 1024th call per window is timed. Samples use **inclusive wall time**, so nested rows overlap and must not be summed. Exception timing excludes the operating system's dispatch/resume cost; high exception frequency remains significant even when handler timings are small. Counts and samples are approximate at window boundaries, and sampled maxima can miss rare stalls.

Profiling is opt-in and checked once during installation. Disabled instrumentation only checks atomic flags; enabled hooks update counters, time sampled calls, and track in-flight calls in a fixed-capacity atomic table. A dedicated diagnostics worker formats and writes the reports independently of configuration polling. Profiling still adds overhead, so use a disabled run to confirm the final FPS result. Performance logs contain metric names, thread IDs and timings, not packet contents or configuration values.

## Mode-transition hangs

The official sources were reviewed at [Black-Ops-3-Projects revision 23536c2, ZBR Native/FPSCounter](https://github.com/shiversoftdev/Black-Ops-3-Projects/tree/23536c20d6488db0282def47b74279d54112fb60/Zombie%20Blood%20Rush%20(Native)/FPSCounter), particularly `protection.cpp` and `dllmain.cpp`. That source revision uses older game offsets and is not a drop-in address map for the recognized September2026 executable. Matching a source baseline is not proof of equivalence with the user's working official binary.

Corrections in this build:

- `check_pending_info` now calls the actual `LobbyMsgRW_PrepReadData` entry (baseline `0x1EEA4F0`, September `0x1EE9E30`). The inherited `0x1EEA150` mapped to `LobbyMsg_HandleIM+0x20`, an internal CALL instruction. Entering there skipped the caller's prologue and continued through it with an invalid stack frame. Installation validates the call relationship and the helper's initializer/tail jump before enabling hooks.
- Steam friend refresh no longer holds the Rust cache mutex while calling Steam. Reentrant/concurrent readers use the last complete snapshot while a refresh is in progress; unknown users remain rejected. Failed refreshes preserve the previous snapshot and allow a retry. Tests exercise a reentrant callback and a concurrent reader.
- Once installation reports ACTIVE, launcher status checks use the four-byte `T7PatchStatus` data export rather than new remote threads and their DLL/TLS initialization callbacks. Waiting-for-initialization retries still use the bootstrap. Parser and process-memory tests cover the new data export and detection of deactivation.
- An invalid script-instance index is rejected before modifying the exception context.

The follow-up crash showed an access violation at September RVA `0x20FC9CF`, followed by `before-NtSuspendProcess`. A read-only inspection of the live game confirmed that instruction is `movzx eax, byte ptr [r9+rax]` in `MSG_ReadByte`; both registers were zero in the crash. A stack return address also matched `LobbyMsg_HandleIM+0xAB`, its integer-field read. The live code exposed the wrong entry above: the E8 at `0x1EE9A90` calls `0x1EE9E30`, whose prologue initializes the reader and tail-jumps to `0x1EEB210` (`PrepReadMsg`). The on-disk game code is protected and was not used to infer those instructions. An older minidump in the game folder described a different exception and was not treated as evidence for this crash.

Regression tests retain the captured 37-byte caller prefix and 42-byte helper, verify their relative targets, and reject the old call-site address. A native execution test replays the captured helper in a test allocation with only its call/jump relocated to local stubs, checking argument forwarding, cursize initialization, return behavior and surrounding canaries. No game functions are called by those tests.

To verify in BO3, restart both programs, launch the new `dist/t7patch.exe`, enter Zombies, load a match, and return to the menu. The newest profile/journal session includes **`lobby_reader=validated-v2`**; the event journal also records `message-reader-validated` with the selected addresses. The entry-point defect is confirmed from live code, while in-game recovery with the rebuilt DLL remains to be confirmed. A signature mismatch is reported as an installation error rather than calling an unverified function.

For a persistent hang, enable profiling as above and check **`hang_tracking=v1`** in the newest session header. Calls lasting at least two seconds produce lines such as `PENDING thread=123 call=steam.owns age_ms=5100`, even if the call was not selected for timing and never returns. Nested calls can appear together. Tracking is bounded to 256 simultaneous calls; `PENDING overflow=...` means some calls could not be tracked. A PENDING line identifies an outstanding call, not proof of deadlock. The independent diagnostics worker can still report a blocked settings worker; it cannot run if the entire process is suspended.

`t7patch-events.log` is created beside `t7patch.conf` during installation, even with profiling disabled. Its pre-opened handle records `unhandled-exception` before detailed crash logging and `before-NtSuspendProcess` immediately before process suspension. Fatal script errors produce `fatal-script` before the existing error dialog. `crashes.log` now uses the same configuration directory, with a PID-specific TEMP fallback if the event journal cannot be opened there. Journal open/write errors also go to `OutputDebugStringA`. The existing known exception recoveries and fatal suspension behavior are retained; a freeze with no `crashes.log` is not assumed to be a recoverable error.

If the freeze remains, collect the last profile windows (wait 15 seconds after the freeze), the newest event-journal session and `crashes.log` if present. This distinguishes an outstanding callback from the patch's explicit fatal path.

## Pending in-game checks

For each recognized executable, on both Windows and Wine/Proton:

1. Open `t7patch.exe` before starting BO3. Check the transition from `No game process found.` through initialization to `Patch active`, without integrity-pattern or MinHook errors. Repeat with the launcher opened after the game has started.
2. Enter the menus, Campaign, Multiplayer, and Zombies. Check player names, localization, UI models, and lobby creation.
3. Test friend invitations, unknown-user filtering, chat, and lobby transitions.
4. Test matching, mismatched, and empty passwords, including changes during a session. Check packet prefixes and the 1500 ms grace period for the previous checksum.
5. Reproduce known crash fixes in a test environment and inspect registers and context. A matching PE fingerprint alone does not establish that every RVA is correct.
6. Deactivate in the menu and verify worker shutdown and restoration of modified entries. Exit the process to finish releasing resources.
7. Record the Wine/Proton version and specifically verify the exception dispatcher path. The assembly sequence comes from the original and depends on that `ntdll` version's layout.
8. Edit the window's fields and verify their effect in the game. Close and reopen the launcher while BO3 is running; exit and relaunch BO3 while keeping the patch window open. Repeat in the same Wine/Proton prefix.

Neither the game nor Wine/Proton has been run during this migration. Verified functional equivalence in those environments is not claimed.
