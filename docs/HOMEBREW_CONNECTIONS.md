# Homebrew: Connections (Google sign-in and the Claude key)

Settings has a "Connections" section (`/settings?section=connections`). It
holds the two outside services this fork signs in to. This note records the
design, the interfaces, the tests, and the limits.

## Google

A personal build has no shared Google registration, so the user registers
their own OAuth client ("Desktop app") in Google Cloud once. The card's guide
(`components/connections/GoogleSetupGuide.tsx`) has four steps; each opens the
Google Cloud page it needs:

1. Create a project; enable the Drive API and the Calendar API.
2. Consent screen: app name, audience External, the app domain links left
   empty; then the user's own address under Audience, "Test users". The
   registration stays in "Testing", where Google ends the sign-in every
   7 days and only listed test users can sign in. "Publish app" removes the
   limit, but Google offers it only once Branding has a public home page and
   a privacy policy on a domain the user owns. The app's message for an ended
   sign-in names the 7-day rule.
3. Create a "Desktop app" client; import the downloaded JSON file, or paste
   the two values. The values are checked (`google/config.rs`); a "Web
   application" client is rejected with the reason.
4. Sign in with Google. Google shows "Google hasn't verified this app" for a
   personal registration. Afterwards the app calls Drive and Calendar once
   (read only) and shows both results.

### Sign-in (`src-tauri/src/google/oauth.rs`)

- System browser, redirect to `http://127.0.0.1:<free port>`, PKCE (S256) and
  a `state` value. A redirect without this attempt's `state` is answered with
  400 and neither finishes nor aborts the attempt.
- Scopes: `drive.file` (only files the app created) and `calendar.readonly`.
  The granted scopes come from the token response, because Google's consent
  page lets single permissions stay unticked.
- Every network call has a timeout. The browser step ends after five minutes,
  on Cancel, or when the user declines.
- `access_token()` refreshes an expiring token under one lock. `invalid_grant`
  or a rejected client is stored as "needs attention" and no further request
  is made until the user signs in again. Outages and network failures are
  transient and keep the connection.
- Disconnect removes the local sign-in and asks Google to revoke it.
- Endpoints are a parameter (`GoogleEndpoints`); tests use a loopback server.

### Storage

Migration `20261124000000_add_integration_config.sql` adds the table
`integration_config` (key, value). `integrations/config_store.rs` is its only
reader. Client secret and tokens are stored in the local database file, as
the app's API keys are. No command returns them: the webview receives
`GoogleStatus` (configured, connected, account address, granted permissions,
needs attention).

### Commands and event

`google_connection_status`, `google_save_client`, `google_remove_client`,
`google_sign_in`, `google_cancel_sign_in`, `google_disconnect`,
`google_test_connection`. Each change emits `google-connection-changed` with
the new `GoogleStatus`. `hooks/useGoogleStatus.ts` keeps views current.

## Claude

`components/connections/ClaudeCard.tsx` with commands in
`summary/claude_commands.rs`: `claude_status`, `claude_models`,
`claude_check_key`, `claude_connect`, `claude_remove_key`.

- The key is tried with one real, minimal request (`claude_api::check_key`)
  before it is saved. The result is "The key works" or the reason in plain
  words (rejected key, billing problem, model not available, unavailable).
- A saved key is never returned to the webview. The card shows that a key
  exists and its last four characters.
- The model defaults to `claude-opus-5-5`.
- Removing the key while Claude writes summaries selects the best downloaded
  local model.

## Prompts

- `ConnectionsWatcher` (mounted in `app/layout.tsx`): one warning toast per
  problem, with an "Open Connections" action.
- `ConnectionsBanner` on the home page while a connection needs attention.
- A failed summary's "open model settings" action now opens Connections (for
  Claude) or the Summaries section; it led nowhere before.

## Secrets in the console

Four `console.log` statements printed configuration objects that carry API
keys (`ModelSettingsModal.tsx`, `SummaryModelSettings.tsx`, two in
`ConfigContext.tsx`). They are removed.

## Tests

- Native: `cargo test -p meetily --lib google::` (client file checks, PKCE
  vectors, authorization address, the whole sign-in against a fake Google with
  a scripted browser, wrong-state redirects, decline, cancel, timeout, refresh
  single-flight, revoked sign-in, outage, disconnect, client change, test
  calls), `integrations::`, `summary::claude_api` (key check),
  `summary::claude_commands`.
- Frontend: `tests/connections/google-card.test.tsx`,
  `claude-card.test.tsx`, `watcher.test.tsx`.

No test contacts Google or Anthropic. The sign-in has not been run against
the real Google from this environment; the first real run is on the user's PC.

## Limits

- Tokens and the client secret are in the app's database file without
  operating-system encryption, like the existing API keys.
- The tray menu and Windows notifications do not show connection problems.
- The exact wording of Google Cloud's pages can change; the guide names the
  buttons as Google's own setup guides describe them at the time of writing.
