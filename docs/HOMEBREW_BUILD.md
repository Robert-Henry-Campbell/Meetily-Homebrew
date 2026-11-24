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
2. Start the build by creating the branch `build/homebrew-installer` from
   `homebrew` (Code tab, branch menu, type the name, "Create branch
   build/homebrew-installer from homebrew"). A push to that branch starts the
   workflow. For a later build, delete the branch and create it again, or
   push the newer `homebrew` commit to it.
3. When the run has finished (about 40 to 70 minutes the first time), open it
   in the Actions tab and download the artifact
   `meetily-homebrew-windows-x64-vulkan`. It contains the installer and
   `SHA256SUMS.txt`.

GitHub shows the "Run workflow" button only for workflows on the default
branch. `main` is the default branch and stays identical to the base, so the
button is absent unless `homebrew` is made the default branch in the
repository settings.

The base's own `build-windows.yml` is not used: it is inherited from upstream,
pins pnpm 8 against a lockfile it cannot read, and compiles the app before the
frontend exists.

State on 2026-11-24: the script and the workflow are written and reviewed
against the base's maintained scripts. They have not run, because Actions was
not enabled on the fork. The first run is the test.

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
