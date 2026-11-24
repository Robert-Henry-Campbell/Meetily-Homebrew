/**
 * The summary that follows a recording is started by the app-level post-call
 * job owner. A meeting page that is already open must show that run.
 */
import React from 'react';
import { act, create, ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';

Object.assign(globalThis, { window: new EventTarget() });
let response: any;
let updates: any[];
let refreshes: number;
let polls: Array<{ meeting: string; owner: string; callback: (result: any) => void }>;
let stopped: Array<[string, string?]>;
// Stable identities, as the real provider gives: the hook's effects depend on them.
const startSummaryPolling = (meeting: string, owner: string, callback: (result: any) => void) => { polls.push({ meeting, owner, callback }); };
const stopSummaryPolling = (meeting: string, owner?: string) => { stopped.push([meeting, owner]); };
mock.module('@/components/Sidebar/SidebarProvider', () => ({ useSidebar: () => ({ startSummaryPolling, stopSummaryPolling }) }));
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string) => {
  if (command === 'api_get_summary') return response;
  throw new Error(`unexpected ${command}`);
} }));
mock.module('@tauri-apps/api/event', () => ({ listen: async () => () => {} }));
mock.module('sonner', () => ({ toast: { success() {}, error() {}, info() {}, warning() {} } }));
mock.module('@/lib/analytics', () => ({ default: {} }));
const { useSummaryGeneration } = await import('../../src/hooks/meeting-details/useSummaryGeneration');
const { SUMMARY_STARTED_EVENT } = await import('../../src/lib/post-call-summary');

let current: ReturnType<typeof useSummaryGeneration>;
let root: ReactTestRenderer;
function Probe({ id }: { id: string }) {
  current = useSummaryGeneration({
    meeting: { id, created_at: '2026-11-23' }, transcripts: [],
    modelConfig: { provider: 'claude', model: 'claude-opus-5-5', whisperModel: 'large-v3' },
    isModelConfigLoading: false, selectedTemplate: 'standard_meeting',
    setAiSummary: (value) => updates.push(value), onMeetingUpdated: async () => { refreshes += 1; },
  });
  return null;
}
const announce = (meetingId: string) => act(async () => {
  window.dispatchEvent(new CustomEvent(SUMMARY_STARTED_EVENT, { detail: { meetingId, processId: 'p1' } }));
});
beforeEach(() => { response = { meeting_id: 'a', status: 'idle', data: null }; updates = []; refreshes = 0; polls = []; stopped = []; });
afterEach(async () => { await act(async () => root?.unmount()); });

test('an open page shows a summary that was started elsewhere and refreshes once when it completes', async () => {
  await act(async () => { root = create(<Probe id="a" />); });
  expect(current.summaryStatus).toBe('idle');
  expect(polls).toHaveLength(0);

  response = { meeting_id: 'a', status: 'processing', data: null };
  await announce('a');
  expect(current.summaryStatus).toBe('processing');
  expect(polls).toHaveLength(1);

  const done = { meeting_id: 'a', status: 'completed', data: { markdown: 'Summary' } };
  await act(async () => { polls[0].callback(done); polls[0].callback(done); });
  expect(current.summaryStatus).toBe('completed');
  expect(updates.at(-1)).toEqual(done.data);
  expect(refreshes).toBe(1);
});

test('a summary started for another meeting is ignored', async () => {
  await act(async () => { root = create(<Probe id="a" />); });
  response = { meeting_id: 'a', status: 'processing', data: null };
  await announce('b');
  expect(current.summaryStatus).toBe('idle');
  expect(polls).toHaveLength(0);
});
