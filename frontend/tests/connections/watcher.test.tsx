import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { CONNECTED, flush, nativeListeners, pushed, reset, text, toasts } from './harness';

const { ConnectionsWatcher } = await import('../../src/components/connections/ConnectionsWatcher');
const { ConnectionsBanner } = await import('../../src/components/connections/ConnectionsBanner');
let root: ReactTestRenderer;
const ENDED = 'Google ended this sign-in (it expired or access was removed). Sign in again.';
const emit = (payload: unknown) => act(async () => nativeListeners.get('google-connection-changed')!({ payload }));

beforeEach(() => reset({ google_connection_status: () => CONNECTED }));
afterEach(async () => { await act(async () => root?.unmount()); });

test('a connection that stops working is announced once, with a button that opens Connections', async () => {
  await act(async () => { root = create(<ConnectionsWatcher />); });
  await flush();
  expect(toasts).toHaveLength(0);

  await emit({ ...CONNECTED, needsAttention: ENDED });
  await emit({ ...CONNECTED, needsAttention: ENDED });
  expect(toasts).toHaveLength(1);
  const [kind, title, options] = toasts[0];
  expect([kind, title, options.description]).toEqual(['warning', 'Google needs your attention', ENDED]);
  options.action.onClick();
  expect(pushed).toEqual(['/settings?section=connections']);

  // Fixed, then broken again later: a new announcement.
  await emit(CONNECTED);
  await emit({ ...CONNECTED, needsAttention: ENDED });
  expect(toasts).toHaveLength(2);
});

test('a problem that already exists at launch is announced, and a Drive permission left unticked counts as one', async () => {
  reset({ google_connection_status: () => ({ ...CONNECTED, driveGranted: false }) });
  await act(async () => { root = create(<ConnectionsWatcher />); });
  await flush();
  expect(toasts).toHaveLength(1);
  expect(toasts[0][2].description).toContain('Drive permission was not ticked');
});

test('the home page shows a line while a connection needs attention and nothing otherwise', async () => {
  await act(async () => { root = create(<ConnectionsBanner />); });
  await flush();
  expect(root.toJSON()).toBeNull();
  await emit({ ...CONNECTED, needsAttention: ENDED });
  const shown = text(root);
  expect(shown).toContain(ENDED);
  expect(shown).toContain('Meetings wait and are sent once this is fixed.');
  expect(root.root.findByType('a').props.href).toBe('/settings?section=connections');
});
