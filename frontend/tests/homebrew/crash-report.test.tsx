import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { button, click, flush, handlers, named, reset, text } from '../connections/harness';

mock.module('@tauri-apps/plugin-dialog', () => ({ save: async () => '/tmp/report.zip' }));
mock.module('@/components/ui/spinner', () => ({ Spinner: () => null }));
mock.module('@tauri-apps/plugin-updater', () => ({ check: async () => { throw new Error('No update check may run in this build'); } }));
mock.module('@tauri-apps/api/app', () => ({ getVersion: async () => '0.2.23' }));

const { default: CrashReportDialog } = await import('../../src/components/CrashReportDialog');
const { CRASH_REPORT_ISSUE_URL, openCrashReportIssue } = await import('../../src/services/crashReportService');
const { UPDATES_ENABLED } = await import('../../src/services/updateService');

const REPORT = { reportId: 'abcdef0123456789', detectedAt: '2026-11-24T01:00:00Z', crashType: 'unexpected_exit' as const, appVersion: '0.2.23' };
let root: ReactTestRenderer;
let resolved = 0;

beforeEach(() => {
  resolved = 0;
  reset({ create_crash_report_zip: ({ destination }: { destination: string }) => destination, dismiss_pending_crash_report: () => undefined });
});
afterEach(async () => { await act(async () => root?.unmount()); });

test('this build reports crashes to nobody: the report can be saved or ignored', async () => {
  expect(CRASH_REPORT_ISSUE_URL).toBeNull();
  await act(async () => { root = create(<CrashReportDialog report={REPORT} onResolved={() => { resolved += 1; }} />); });
  await flush();
  expect(button(root, 'Send Report')).toBeUndefined();
  expect(text(root)).toContain('Nothing is sent anywhere.');
  expect(text(root)).not.toContain('GitHub');

  await click(root, 'Save ZIP');
  expect(named('create_crash_report_zip')[0][1]).toEqual({ destination: '/tmp/report.zip' });
  expect(named('open_external_url')).toEqual([]);
  expect(resolved).toBe(1);
});

test('nothing opens the base project tracker, even when asked directly', async () => {
  await openCrashReportIssue(REPORT);
  expect(named('open_external_url')).toEqual([]);
});

test('updates stay switched off: a forced check asks no release feed', async () => {
  expect(UPDATES_ENABLED).toBe(false);
  const { updateService } = await import('../../src/services/updateService');
  expect(await updateService.checkForUpdates(true)).toEqual({ available: false, currentVersion: '0.2.23' });
});
