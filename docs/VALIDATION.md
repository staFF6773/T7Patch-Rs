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

## Automated checks

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
