# Homebrew: branches, identity, and the Windows installer

## Branches

| Branch | Content |
| --- | --- |
| `main` | Identical to the base, `TylerBuza/Meetily-ActuallyFree` at `613f375`. |
| `feat/auto-chain` | The chain after Stop, large-v3, Claude request, local takeover. |
| `feat/connections` | Connections section, Google sign-in, checked Claude key. Builds on the previous branch. |
| `feat/drive-export` | Google Drive export. Builds on the previous branch. |
| `feat/google-calendar` | Google Calendar matching. Builds on the previous branch. |
| `homebrew` | All of the above plus the separate app identity and the installer workflow. Install from this branch. |

Feature notes: [HOMEBREW_AUTO_CHAIN.md](HOMEBREW_AUTO_CHAIN.md),
[HOMEBREW_CONNECTIONS.md](HOMEBREW_CONNECTIONS.md),
[HOMEBREW_DRIVE_EXPORT.md](HOMEBREW_DRIVE_EXPORT.md),
[HOMEBREW_CALENDAR.md](HOMEBREW_CALENDAR.md).

## A separate app

The `homebrew` branch builds an app that installs beside a stock Meetily and
shares nothing with it:

| | Stock | Homebrew |
| --- | --- | --- |
| Name | Meetily - Actually Free | Meetily Homebrew |
| Identifier | `com.meetily.ai` | `com.meetily.homebrew` |
| Executable | `meetily.exe` | `meetily-homebrew.exe` |
| Install folder | `%LOCALAPPDATA%\Meetily-ActuallyFree` | `%LOCALAPPDATA%\Meetily-Homebrew` |
| Data (database, models, templates) | `data\` in its install folder; `%APPDATA%\Meetily` if that is not writable | `data\` in its own install folder; `%APPDATA%\Meetily Homebrew` if that is not writable |
| Default recordings folder | `Music\meetily-recordings` | `Music\meetily-homebrew-recordings` |

Reasons for each difference:

- The database of this build has tables and triggers the stock build does not
  know. sqlx refuses to open a database with migrations it does not have, so
  the two must never share a data folder. Three names keep them apart: the
  install folder in `windows/installer.nsi` (the base's template names the
  stock folder literally), the fallback folder in `src/paths.rs`, and the
  recordings folder in `src/audio/recording_preferences.rs`.
- Nothing is copied from a stock install. This build starts with no meetings
  and downloads its own models.
- The installer and the uninstaller close a running app by executable name.
  With its own name, installing or removing this build never closes a stock
  Meetily that is recording.
- Updates are switched off (`services/updateService.ts`), and the updater
  endpoint points at this fork, which publishes no releases. An update from
  the base project's feed would replace this app with the stock one.
- The "CUDA is ready" download offer is removed; it pointed at the base
  project's installer.
- First-run setup does not ask about update checks, since none run.
- The crash dialog offers "Save ZIP" and "Ignore" only. The base's "Send
  Report" opens a new issue on the base project; crashes of this build come
  from code that project did not write, and this fork has no issue tracker
  (`services/crashReportService.ts`, `CRASH_REPORT_ISSUE_URL`).

The version stays `0.2.23`. A pre-release suffix was planned and left out,
because the custom installer template has not been tried with one.

## Windows installer (Vulkan)

`frontend/scripts/build-homebrew-windows.ps1` builds one unsigned NSIS
installer with the Vulkan backend (AMD, Intel and compatible NVIDIA graphics).
It follows the base's `build-labs-test-windows.ps1` and the Vulkan part of
`build-universal-windows.ps1`, without the CPU and CUDA variants, the
bootstrapper, code signing and updater artifacts.

`.github/workflows/homebrew-windows.yml` runs it on `windows-2022`:

1. GitHub, the fork, tab Actions. If workflows are disabled, enable them
   ("I understand my workflows, go ahead and enable them").
2. A push to the branch `build/homebrew-installer` starts the workflow. The
   branch exists; for a new build, push the newer `homebrew` commit to it
   (`git push origin homebrew:build/homebrew-installer`), or delete the branch
   in the GitHub web interface and create it again from `homebrew`.
3. When the run has finished (about 22 minutes with the build cache, about
   40 without), open it in the Actions tab and download the artifact
   `meetily-homebrew-windows-x64-vulkan`. It is a ZIP with the installer and
   `SHA256SUMS.txt`. GitHub keeps it for 30 days.

GitHub shows the "Run workflow" button only for workflows on the default
branch. `main` is the default branch and stays identical to the base, so the
button is absent unless `homebrew` is made the default branch in the
repository settings.

The base's own `build-windows.yml` is not used: it is inherited from upstream,
pins pnpm 8 against a lockfile it cannot read, and compiles the app before the
frontend exists.

After the installer is uploaded, the workflow runs
`frontend/scripts/smoke-test-homebrew-windows.ps1` on the build machine:

1. Silent install (`/S`).
2. The expected files are in `%LOCALAPPDATA%\Meetily-Homebrew`, the app is
   registered for removal as "Meetily Homebrew", and no
   `%LOCALAPPDATA%\Meetily-ActuallyFree` folder appeared.
3. The app is started and has to create `data\meeting_minutes.sqlite` within
   90 seconds and report "Fresh database initialized successfully" in its
   log. The window starts that on a first start and the app logs the line
   after the migrations, so it shows that the embedded frontend loaded in
   WebView2 and every migration applied. The
   program loads `vulkan-1.dll` at start; the build machine has no graphics
   driver, so the test fetches the loader from LunarG's runtime components.
   If no loader can be had, the start check is skipped with a warning.
4. Silent uninstall.

A failed smoke test turns the run red and leaves the installer downloadable.
The script refuses to run on a machine where this app or a stock Meetily is
installed.

The build log also lists the DLLs the program loads at start
("DLL imports of the app"). `vulkan-1.dll` is in that list: the app starts
only on a PC whose graphics driver provides the Vulkan loader; current AMD
and Intel drivers do. On a PC without it, Windows reports that
`vulkan-1.dll` was not found when the app is started.

### LLVM 18

The base's scripts generate the whisper.cpp bindings with LLVM 18. GitHub's
`windows-2022` machines ship a newer LLVM (20.1.8 in November 2026), so the
workflow runs `frontend/scripts/install-llvm18-windows.ps1` before the build.
It downloads the official `LLVM-18.1.8-win64.exe` with a time limit, checks a
pinned SHA-256, and unpacks `libclang.dll`, `clang.exe` and clang's headers
with 7-Zip (256 MB instead of 2.8 GB). The build script reads the folder from
`MEETILY_LLVM_DIR`.

The base's own helper, `bootstrap-llvm18.ps1`, is not used here. In the first
run of this workflow it printed "Downloading portable LLVM 18" and then
nothing for three hours, until the job's limit. It fetches a `.tar.xz` with
`gh release download` and unpacks it with Windows `tar`; neither has a time
limit, and the `tar` of Windows Server 2022 cannot read xz by itself. The log
does not show which of the two stalled.

On a build machine the build script uses no fallback helper at all: a missing
LLVM 18, or a Vulkan SDK without `glslc.exe`, stops the build at once with a
message. Every long step of the workflow has a time limit.

### Rust version and ONNX Runtime

The workflow installs Rust 1.97.0, the version this branch is built and tested
with on Linux, instead of the newest release. The build script also sets
`ORT_LIB_LOCATION` to `frontend\src-tauri\binaries\onnxruntime`, as the
base's `build-labs-test-windows.ps1` does: the app loads ONNX Runtime at run
time from that folder, which its own build script stages and verifies, so the
`ort-sys` package has nothing to download or copy during the build.

### NSIS progress plugin

The base's `build-nsis-progress-plugin.ps1` is not run. The installer template
uses that plugin only in update mode (`/UPDATE`), and this build has no
updates; a normal install shows its progress without it. On a machine that
has never bundled with Tauri the script also fails, because it downloads NSIS
from an address that answers "Not Found". Tauri's bundler fetches its own NSIS.

### Runs

| Date | Result |
| --- | --- |
| 2026-11-24, first run | Cancelled by the three-hour limit in the LLVM download described above. All earlier steps passed. |
| 2026-11-24, second run | LLVM 18 installed in 19 seconds; the Next.js build and the summary helper compiled. The app build stopped in the build script of `ort-sys` (ONNX Runtime bindings). The build machine had Rust 1.100, whose cargo keeps build folders as `build\<package>\<hash>` instead of `build\<package>-<hash>`; `ort-sys` derives the folder for its DLL copies from that position and found none. |
| 2026-11-24, third run | With Rust 1.97.0 and `ORT_LIB_LOCATION` the app compiled for Windows (16 minutes) and its imports were listed; `vulkan-1.dll` is among them. The run stopped in the base's NSIS progress plugin script, at its NSIS download. |
| 2026-11-24, fourth run | The installer was built and uploaded (16 minutes with the cache). Smoke test: the silent install passed, the files were in `%LOCALAPPDATA%\Meetily-Homebrew`, and no stock folder appeared. The start check failed with "DLL not found" (exit code -1073741515): the build machine has no Vulkan loader and the Vulkan SDK step provides none. |
| 2026-11-24, fifth run | Green. With the loader from LunarG's runtime components (signature valid) the installed app started on the build machine, loaded ONNX Runtime 1.22.0 and created its database after about 10 seconds. The silent uninstall left only `data` and `uninstall.exe`. |
| 2026-11-24, sixth run | Green, with the stricter start check: the app's log shows "Database opened successfully" and "Fresh database initialized successfully" on Windows. Built from commit `5830231`; installer SHA-256 `946c790e4a28654c6a91ceae82af9054bd62e2666ffab21871d1aae160963ddc`. |

What these runs do not cover, because the build machine has no microphone,
no graphics driver and no accounts: recording, transcription with Whisper
large-v3 on real graphics, speaker labels, summaries through Claude, and the
Google sign-in with Drive and Calendar. The checks on the PC below cover them.

The installer is unsigned. Windows SmartScreen shows "Windows protected your
PC"; choose "More info", then "Run anyway". The first start downloads about
6 GB of models (Parakeet 0.7, Whisper large-v3 3.0, local summary model 2.6).

## Checks on the PC

1. Install, finish setup (leave "Whisper Large v3" ticked).
2. Settings, Connections: paste the Claude key and choose "Check and save".
3. Settings, Connections: "Connect Google" and follow the four steps.
4. Record a one-minute call. After Stop, answer "How many people spoke?".
   Transcript, speaker labels, summary and the two Drive files follow without
   another click. The meeting shows "In Drive".
5. Quit the app during "Improving the transcript" once and start it again: the
   chain continues.
6. Record during a calendar event: the meeting takes the event's title.

## Browser preview

`src/dev/preview/index.ts` (development only) has sample data for the new
screens. With `pnpm exec next dev -p 3118` in `frontend/`:
`http://localhost:3118/settings?section=connections` (not connected) and
`...&preview=google` (signed in).
