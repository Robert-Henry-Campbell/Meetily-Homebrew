# Characterisation of upstream PR #666

Upstream PR #666 (`Zackriya-Solutions/meetily`, commit `d371203`) adds a Google
Calendar integration. This small crate compiles that PR's `calendar/client.rs`
and `calendar/matcher.rs` exactly as published and states, as tests, what the
code does. The PR's files are fetched, not committed.

```bash
cd tools/pr666-characterisation
```

```bash
mkdir -p pr666 && for f in mod client matcher; do curl -sS -o pr666/$f.rs "https://raw.githubusercontent.com/Zackriya-Solutions/meetily/d371203/frontend/src-tauri/src/calendar/$f.rs"; done
```

```bash
cargo test
```

Result on 2026-11-24: 8 tests pass. Two are the PR's own; six are added here
and assert the behaviour listed in `docs/HOMEBREW_CALENDAR.md`.
