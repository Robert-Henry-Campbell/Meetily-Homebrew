/**
 * Connections to outside services: Google (Drive and Calendar) and Claude.
 *
 * The native side keeps every secret. These calls return what the screen may
 * show: whether something is connected, the account's address, a hint of the
 * saved key, and the result of a real test call.
 */
import { invoke } from '@tauri-apps/api/core';

export interface GoogleStatus {
  /** An OAuth client was imported (guide steps 1 to 3). */
  clientConfigured: boolean;
  clientIdHint: string | null;
  connected: boolean;
  accountEmail: string | null;
  /** Google's consent page lets single permissions stay unticked. */
  driveGranted: boolean;
  calendarGranted: boolean;
  /** Set when Google ended the stored sign-in; the user has to sign in again. */
  needsAttention: string | null;
}

export interface CheckResult {
  ok: boolean;
  message: string;
}

export interface GoogleCheck {
  drive: CheckResult;
  calendar: CheckResult;
}

export interface ClaudeStatus {
  keySaved: boolean;
  /** The end of the saved key, to recognise it by. */
  keyHint: string | null;
  /** Claude is the provider that writes summaries. */
  selected: boolean;
  model: string;
}

/** Emitted natively with a `GoogleStatus` whenever the connection changes. */
export const GOOGLE_CONNECTION_CHANGED = 'google-connection-changed';
export const CONNECTIONS_ROUTE = '/settings?section=connections';
/** Keep in step with `claude_api::DEFAULT_CLAUDE_MODEL`. */
export const DEFAULT_CLAUDE_MODEL = 'claude-opus-5-5';

export const EMPTY_GOOGLE_STATUS: GoogleStatus = {
  clientConfigured: false,
  clientIdHint: null,
  connected: false,
  accountEmail: null,
  driveGranted: false,
  calendarGranted: false,
  needsAttention: null,
};

export const describeError = (cause: unknown): string =>
  cause instanceof Error ? cause.message : String(cause);

export const getGoogleStatus = () => invoke<GoogleStatus>('google_connection_status');

/** Step 3: the text of the downloaded JSON file, or the two pasted values. */
export const saveGoogleClient = (input: { json?: string; clientId?: string; clientSecret?: string }) =>
  invoke<GoogleStatus>('google_save_client', {
    json: input.json ?? null,
    clientId: input.clientId ?? null,
    clientSecret: input.clientSecret ?? null,
  });

export const removeGoogleClient = () => invoke<GoogleStatus>('google_remove_client');
/** Step 4: resolves when the browser sign-in finished; rejects with the reason otherwise. */
export const signInToGoogle = () => invoke<GoogleStatus>('google_sign_in');
export const cancelGoogleSignIn = () => invoke<void>('google_cancel_sign_in');
export const disconnectGoogle = () => invoke<GoogleStatus>('google_disconnect');
export const testGoogleConnection = () => invoke<GoogleCheck>('google_test_connection');

export const getClaudeStatus = () => invoke<ClaudeStatus>('claude_status');
/** A real, minimal request. Without `apiKey` the saved key is tried. */
export const checkClaudeKey = (apiKey: string | null, model: string) =>
  invoke<CheckResult>('claude_check_key', { apiKey, model });
export const connectClaude = (apiKey: string | null, model: string) =>
  invoke<ClaudeStatus>('claude_connect', { apiKey, model });
export const removeClaudeKey = () => invoke<ClaudeStatus>('claude_remove_key');

export const openExternal = (url: string) => invoke<void>('open_external_url', { url });

/** What a status line or a prompt says about Google, most urgent first. */
export function googleAttention(status: GoogleStatus): string | null {
  if (status.needsAttention) return status.needsAttention;
  if (status.connected && !status.driveGranted) {
    return 'The Drive permission was not ticked on Google\'s page, so meetings cannot be sent to Drive. Sign in again and tick it.';
  }
  return null;
}

/** Google Cloud pages the setup guide opens. The addresses are the ones Google's own guides link to. */
export const GOOGLE_CLOUD_LINKS = {
  newProject: 'https://console.cloud.google.com/projectcreate',
  enableApis: 'https://console.cloud.google.com/apis/enableflow;apiid=drive.googleapis.com,calendar-json.googleapis.com',
  branding: 'https://console.developers.google.com/auth/branding',
  audience: 'https://console.developers.google.com/auth/audience',
  clients: 'https://console.developers.google.com/auth/clients',
} as const;
