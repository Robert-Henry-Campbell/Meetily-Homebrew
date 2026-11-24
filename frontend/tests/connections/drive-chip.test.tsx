import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { button, click, flush, handlers, named, nativeListeners, reset, text } from './harness';

const { DriveChip } = await import('../../src/components/meeting/DriveChip');
const { DriveExportOptions } = await import('../../src/components/connections/DriveExportOptions');
let root: ReactTestRenderer;
const status = (state: string, extra: Record<string, unknown> = {}) => ({
  state, message: null, summaryLink: null, transcriptLink: null, folderLink: null, exportedAt: null, ...extra,
});
const mount = async (id = 'm') => { await act(async () => { root = create(<DriveChip meetingId={id} />); }); await flush(); };
const SENT = status('sent', {
  summaryLink: 'https://drive.google.com/file/d/s/view',
  transcriptLink: 'https://drive.google.com/file/d/t/view',
  folderLink: 'https://drive.google.com/drive/folders/f',
  exportedAt: 1795000000,
});

beforeEach(() => reset({ open_external_url: () => undefined }));
afterEach(async () => { await act(async () => root?.unmount()); });

test('a sent meeting links to its folder and both files', async () => {
  handlers.drive_export_status = () => SENT;
  await mount();
  expect(text(root)).toContain('In Drive');
  for (const label of ['Open the folder', 'Open the summary', 'Open the transcript']) await click(root, label);
  expect(named('open_external_url').map(([, args]) => args.url)).toEqual([
    'https://drive.google.com/drive/folders/f',
    'https://drive.google.com/file/d/s/view',
    'https://drive.google.com/file/d/t/view',
  ]);
  expect(button(root, 'Send again')).toBeDefined();
});

test('a waiting meeting says what it waits for and can be sent now', async () => {
  handlers.drive_export_status = () => status('waiting', { message: 'The summary is being written.' });
  handlers.drive_export_send_now = () => status('waiting', { message: 'Sending to Drive shortly.' });
  await mount();
  expect(text(root)).toContain('Drive: waiting');
  expect(text(root)).toContain('The summary is being written.');
  // Only the links that exist are offered.
  expect(button(root, 'Open the summary')).toBeUndefined();
  await click(root, 'Send now');
  expect(named('drive_export_send_now')[0][1]).toEqual({ meetingId: 'm' });
  expect(text(root)).toContain('Sending to Drive shortly.');
});

test('the chip follows the background export and the connection without being reopened', async () => {
  let current = status('waiting', { message: 'Sending to Drive shortly.' });
  handlers.drive_export_status = () => current;
  await mount();
  current = SENT;
  // Another meeting's change is not this chip's business.
  await act(async () => nativeListeners.get('drive-export-changed')!({ payload: { meetingId: 'other' } }));
  await flush();
  expect(text(root)).toContain('Drive: waiting');
  await act(async () => nativeListeners.get('drive-export-changed')!({ payload: { meetingId: 'm' } }));
  await flush();
  expect(text(root)).toContain('In Drive');

  current = status('not-connected');
  await act(async () => nativeListeners.get('google-connection-changed')!({ payload: {} }));
  await flush();
  expect(text(root)).toContain('Drive not connected');
});

test('problems name the cause and lead to the place to fix them', async () => {
  handlers.drive_export_status = () => status('attention', { message: 'Google ended this sign-in. Sign in again.' });
  await mount();
  expect(text(root)).toContain('Drive needs attention');
  expect(text(root)).toContain('Google ended this sign-in. Sign in again.');
  expect(root.root.findByType('a').props.href).toBe('/settings?section=connections');
  expect(button(root, 'Send now')).toBeDefined();
  await act(async () => root.unmount());

  handlers.drive_export_status = () => status('not-connected');
  await mount();
  expect(text(root)).toContain('Connect Google to send');
  expect(root.root.findByType('a').props.href).toBe('/settings?section=connections');
  expect(button(root, 'Send now')).toBeUndefined();
  await act(async () => root.unmount());

  handlers.drive_export_status = () => status('none');
  await mount();
  expect(text(root)).toContain('Not in Drive');
  expect(text(root)).toContain('recorded before the Drive export was switched on');
  expect(button(root, 'Send to Drive')).toBeDefined();
});

test('the export options are saved one change at a time, and the folder name on request', async () => {
  let saved = { autoExport: true, folderPerPerson: true, rootFolderName: 'Meetily meetings', enabledSince: 1 };
  reset({
    drive_export_settings: () => saved,
    drive_export_save_settings: (args) => { saved = { ...saved, ...args, rootFolderName: args.rootFolderName.trim() }; return saved; },
  });
  await act(async () => { root = create(<DriveExportOptions />); });
  await flush();
  const toggle = (label: string) => root.root.findByProps({ role: 'switch', 'aria-label': label });
  expect(toggle('Send meetings to Drive automatically').props['aria-checked']).toBe(true);

  await act(async () => toggle('A folder per person').props.onClick());
  await flush();
  expect(named('drive_export_save_settings')[0][1]).toEqual({ autoExport: true, folderPerPerson: false, rootFolderName: 'Meetily meetings' });
  expect(toggle('A folder per person').props['aria-checked']).toBe(false);

  expect(button(root, 'Save')!.props.disabled).toBe(true);
  await act(async () => root.root.findByProps({ 'aria-label': 'Name of the main Drive folder' }).props.onChange({ target: { value: ' Meetings 2026 ' } }));
  await click(root, 'Save');
  expect(named('drive_export_save_settings')[1][1]).toEqual({ autoExport: true, folderPerPerson: false, rootFolderName: ' Meetings 2026 ' });
  expect(root.root.findByProps({ 'aria-label': 'Name of the main Drive folder' }).props.value).toBe('Meetings 2026');
});
