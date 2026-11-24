import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { button, calls, click, CLIENT_ONLY, CONNECTED, flush, handlers, named, nativeListeners, NOT_CONNECTED, reset, text } from './harness';

const { GoogleCard } = await import('../../src/components/connections/GoogleCard');
let root: ReactTestRenderer;
const mount = async () => { await act(async () => { root = create(<GoogleCard />); }); await flush(); };
const field = (label: string) => root.root.findByProps({ 'aria-label': label });

const DRIVE_OPTIONS = { autoExport: true, folderPerPerson: true, rootFolderName: 'Meetily meetings', enabledSince: 1 };
const CALENDARS = { matching: true, calendars: [
  { id: 'me@example.com', name: 'me@example.com', primary: true, selected: true },
  { id: 'team@group.calendar.google.com', name: 'Team', primary: false, selected: false },
] };
beforeEach(() => reset({
  google_connection_status: () => NOT_CONNECTED, open_external_url: () => undefined,
  drive_export_settings: () => DRIVE_OPTIONS, calendar_options: () => CALENDARS,
}));
afterEach(async () => { await act(async () => root?.unmount()); });

test('an unconnected card offers the guide, whose buttons open the Google Cloud pages for each step', async () => {
  await mount();
  expect(text(root)).toContain('Not connected');
  expect(root.root.findAllByProps({ role: 'dialog' })).toHaveLength(0);

  await click(root, 'Connect Google');
  const guide = text(root);
  for (const title of ['Create a project and switch on two services', 'Set up the consent screen and publish it', 'Create a Desktop client and import it here', 'Sign in with Google']) {
    expect(guide).toContain(title);
  }
  // The points that are easy to get wrong are stated.
  expect(guide).toContain('Desktop app');
  expect(guide).toContain('Publish app');
  expect(guide).toContain('every 7 days');
  expect(guide).toContain('only the files it creates itself');
  expect(guide).toContain('read only');

  for (const label of ['Create a project', 'Enable Drive and Calendar', 'Open Branding', 'Open Audience', 'Open Clients']) await click(root, label);
  expect(named('open_external_url').map(([, args]) => args.url)).toEqual([
    'https://console.cloud.google.com/projectcreate',
    'https://console.cloud.google.com/apis/enableflow;apiid=drive.googleapis.com,calendar-json.googleapis.com',
    'https://console.developers.google.com/auth/branding',
    'https://console.developers.google.com/auth/audience',
    'https://console.developers.google.com/auth/clients',
  ]);
  // Sign-in is not offered before a client exists.
  expect(button(root, 'Sign in with Google')!.props.disabled).toBe(true);
});

test('the downloaded JSON file is imported as it is, and a rejected file shows the reason', async () => {
  const file = '{"installed":{"client_id":"123-abc.apps.googleusercontent.com","client_secret":"GOCSPX-secret"}}';
  handlers.google_save_client = () => { throw new Error('This client has the type "Web application".'); };
  await mount();
  await click(root, 'Connect Google');
  const input = field('Google client JSON file');
  await act(async () => { await input.props.onChange({ target: { files: [{ size: file.length, text: async () => file }], value: 'x' } }); });
  await flush();
  expect(named('google_save_client')[0][1]).toEqual({ json: file, clientId: null, clientSecret: null });
  expect(text(root)).toContain('Web application');

  handlers.google_save_client = () => CLIENT_ONLY;
  await act(async () => { await input.props.onChange({ target: { files: [{ size: file.length, text: async () => file }], value: 'x' } }); });
  await flush();
  expect(text(root)).toContain('Imported: 12345678….apps.googleusercontent.com');
  expect(text(root)).not.toContain('Web application');
  // The secret is sent to the native side and is not kept on screen.
  expect(text(root)).not.toContain('GOCSPX-secret');
  expect(button(root, 'Sign in with Google')!.props.disabled).toBe(false);

  // A file that cannot be a client file is not read at all.
  const before = named('google_save_client').length;
  await act(async () => { await input.props.onChange({ target: { files: [{ size: 5_000_000, text: async () => 'big' }], value: 'x' } }); });
  expect(named('google_save_client')).toHaveLength(before);
  expect(text(root)).toContain('too large');
});

test('the two values can be pasted instead of importing the file', async () => {
  handlers.google_save_client = () => CLIENT_ONLY;
  await mount();
  await click(root, 'Connect Google');
  await click(root, 'Paste the values instead');
  await act(async () => field('Google client ID').props.onChange({ target: { value: '123-abc.apps.googleusercontent.com' } }));
  await act(async () => field('Google client secret').props.onChange({ target: { value: 'GOCSPX-secret' } }));
  expect(field('Google client secret').props.type).toBe('password');
  await click(root, 'Save the client');
  expect(named('google_save_client')[0][1]).toEqual({ json: null, clientId: '123-abc.apps.googleusercontent.com', clientSecret: 'GOCSPX-secret' });
});

test('signing in waits for the browser, then tests both services and shows each result', async () => {
  let finish!: (status: unknown) => void;
  reset({
    google_connection_status: () => CLIENT_ONLY,
    google_sign_in: () => new Promise((resolve) => { finish = resolve; }),
    google_cancel_sign_in: () => undefined,
    drive_export_settings: () => DRIVE_OPTIONS,
    calendar_options: () => CALENDARS,
    google_test_connection: () => ({
      drive: { ok: true, message: 'Drive works.' },
      calendar: { ok: false, message: 'This API is not enabled in your Google Cloud project (step 1).' },
    }),
  });
  await mount();
  await click(root, 'Continue the setup');
  await act(async () => { void button(root, 'Sign in with Google')!.props.onClick(); });
  expect(text(root)).toContain('Waiting for the browser…');
  expect(button(root, 'Cancel')).toBeDefined();

  await act(async () => finish(CONNECTED));
  await flush();
  const shown = text(root);
  expect(shown).toContain('Connected as person@example.com');
  expect(shown).toContain('Drive works.');
  expect(shown).toContain('not enabled in your Google Cloud project (step 1)');
  expect(named('google_test_connection')).toHaveLength(1);
});

test('a sign-in that was declined shows why and can be cancelled while it waits', async () => {
  reset({
    google_connection_status: () => CLIENT_ONLY,
    google_sign_in: () => { throw new Error("Access was not granted on Google's page."); },
    google_cancel_sign_in: () => undefined,
  });
  await mount();
  await click(root, 'Continue the setup');
  await click(root, 'Sign in with Google');
  expect(text(root)).toContain("Access was not granted on Google's page.");
  expect(named('google_test_connection')).toHaveLength(0);

  let reject!: (error: Error) => void;
  handlers.google_sign_in = () => new Promise((_, no) => { reject = no; });
  await act(async () => { void button(root, 'Sign in with Google')!.props.onClick(); });
  await click(root, 'Cancel');
  expect(named('google_cancel_sign_in')).toHaveLength(1);
  await act(async () => reject(new Error('Sign-in was cancelled.')));
  expect(text(root)).toContain('Sign-in was cancelled.');
});

test('a connected card shows the account and permissions, can be tested, and disconnects after confirmation', async () => {
  reset({
    google_connection_status: () => ({ ...CONNECTED, calendarGranted: false }),
    drive_export_settings: () => DRIVE_OPTIONS,
    calendar_options: () => CALENDARS,
    google_test_connection: () => ({ drive: { ok: true, message: 'Drive works.' }, calendar: { ok: false, message: 'The Calendar permission was not ticked.' } }),
    google_disconnect: () => CLIENT_ONLY,
  });
  await mount();
  const shown = text(root);
  expect(shown).toContain('Signed in as person@example.com');
  expect(shown).toContain('Connected');
  expect(shown).toContain('Allowed');
  expect(shown).toContain('Not allowed');
  // The export options appear once Drive may be used.
  expect(shown).toContain('Send meetings to Drive automatically');

  await click(root, 'Test the connection');
  expect(text(root)).toContain('Drive works.');

  await click(root, 'Disconnect');
  expect(named('google_disconnect')).toHaveLength(0);
  expect(text(root)).toContain('Files already in your Drive stay there');
  await click(root, 'confirm:Disconnect');
  expect(named('google_disconnect')).toHaveLength(1);
  expect(text(root)).toContain('Not signed in');
  expect(button(root, 'Remove the imported client')).toBeDefined();
});

test('a sign-in that Google ended is shown as needing attention, with one button to fix it', async () => {
  reset({
    google_connection_status: () => ({ ...CONNECTED, needsAttention: 'Google ended this sign-in (it expired or access was removed). Sign in again.' }),
    drive_export_settings: () => DRIVE_OPTIONS,
    calendar_options: () => CALENDARS,
  });
  await mount();
  expect(text(root)).toContain('Needs attention');
  expect(text(root)).toContain('Google ended this sign-in');
  await click(root, 'Sign in again');
  expect(root.root.findAllByProps({ role: 'dialog' })).toHaveLength(1);
});

test('a change made elsewhere reaches the open card', async () => {
  await mount();
  expect(text(root)).toContain('Not connected');
  await act(async () => nativeListeners.get('google-connection-changed')!({ payload: CONNECTED }));
  expect(text(root)).toContain('Signed in as person@example.com');
  // No command returned or displayed a token or secret at any point.
  expect(JSON.stringify(calls)).not.toMatch(/refresh|access_token|GOCSPX/);
});

test('calendars are listed once Calendar may be used, and ticking one saves the choice', async () => {
  let saved: any = null;
  reset({
    google_connection_status: () => CONNECTED,
    drive_export_settings: () => DRIVE_OPTIONS,
    calendar_options: () => CALENDARS,
    calendar_save_options: (args) => { saved = args; },
  });
  await mount();
  expect(text(root)).toContain('Match recordings to calendar events');
  expect(text(root)).toContain('Events are only read');
  const boxes = root.root.findAllByProps({ type: 'checkbox' });
  expect(boxes.map((box) => box.props.checked)).toEqual([true, false]);
  await act(async () => boxes[1].props.onChange({ target: { checked: true } }));
  await flush();
  expect(saved).toEqual({ matching: true, calendarIds: ['me@example.com', 'team@group.calendar.google.com'] });

  // Switched off: the list is hidden and nothing is matched.
  await act(async () => root.root.findByProps({ role: 'switch', 'aria-label': 'Match recordings to calendar events' }).props.onClick());
  await flush();
  expect(saved.matching).toBe(false);
  expect(root.root.findAllByProps({ type: 'checkbox' })).toHaveLength(0);
});

test('a calendar list that cannot be read says why and can be tried again', async () => {
  let fail = true;
  reset({
    google_connection_status: () => CONNECTED,
    drive_export_settings: () => DRIVE_OPTIONS,
    calendar_options: () => { if (fail) throw new Error('The Google Calendar API is not enabled in your Google Cloud project (step 1 of the setup guide).'); return CALENDARS; },
  });
  await mount();
  expect(text(root)).toContain('step 1 of the setup guide');
  fail = false;
  await click(root, 'Try again');
  await flush();
  expect(root.root.findAllByProps({ type: 'checkbox' })).toHaveLength(2);

  // Without the Calendar permission the options are not offered at all.
  reset({ google_connection_status: () => ({ ...CONNECTED, calendarGranted: false }), drive_export_settings: () => DRIVE_OPTIONS });
  await act(async () => root.unmount());
  await mount();
  expect(text(root)).not.toContain('Match recordings to calendar events');
});
