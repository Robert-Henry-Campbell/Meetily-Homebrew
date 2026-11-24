/**
 * The summary starter used by the post-call job owner. Each outcome decides
 * whether the stored job is finished, retried later, or reported.
 */
import { beforeEach, expect, mock, test } from 'bun:test';

const calls: Array<[string, any]> = [];
let handlers: Record<string, (args: any) => unknown>;
const local = new Map<string, string>();
const started: any[] = [];
const target = new EventTarget();
Object.assign(globalThis, {
  window: target,
  localStorage: { getItem: (key: string) => local.get(key) ?? null, setItem: (key: string, value: string) => local.set(key, value) },
});
mock.module('@tauri-apps/api/core', () => ({ invoke: async (name: string, args: any) => {
  calls.push([name, args]);
  return handlers[name]?.(args);
} }));
mock.module('@/lib/summary-language-resolution', () => ({ resolveSummaryLanguage: async () => 'en' }));

const { startPostCallSummary, SUMMARY_STARTED_EVENT } = await import('../../src/lib/post-call-summary');
target.addEventListener(SUMMARY_STARTED_EVENT, (event) => started.push((event as CustomEvent).detail));

const rows = [
  { id: '1', text: 'um so the the budget is fine', timestamp: '10:00', audio_start_time: 0, speaker: 'You' },
  { id: '2', text: 'agreed', timestamp: '10:01', speaker: undefined },
];
const sent = () => calls.filter(([name]) => name === 'api_process_transcript');

beforeEach(() => {
  calls.length = 0; started.length = 0; local.clear();
  handlers = {
    api_get_summary: () => ({ status: 'idle', data: null }),
    api_get_model_config: () => ({ provider: 'claude', model: 'claude-opus-5-5' }),
    api_get_meeting_transcripts: () => ({ transcripts: rows, total_count: rows.length, has_more: false }),
    api_process_transcript: () => ({ process_id: 'p1' }),
  };
});

test('starts the summary with the saved model and the same line format as the meeting page', async () => {
  const outcome = await startPostCallSummary('m');
  expect(outcome).toEqual({ status: 'started', processId: 'p1', provider: 'claude', model: 'claude-opus-5-5' });
  expect(sent()[0][1]).toEqual({
    text: '[00:00] You: um so the the budget is fine\n10:01 agreed',
    model: 'claude',
    modelName: 'claude-opus-5-5',
    meetingId: 'm',
    chunkSize: 40000,
    overlap: 1000,
    customPrompt: '',
    templateId: 'standard_meeting',
    summaryLanguage: 'en',
  });
  // An open meeting page learns about a run it did not start.
  expect(started).toEqual([{ meetingId: 'm', processId: 'p1' }]);
});

test('writes from the clean transcript when that Labs switch is on', async () => {
  local.set('meetily-labs-v1', JSON.stringify({ cleanTranscript: true }));
  await startPostCallSummary('m');
  expect(sent()[0][1].text).toBe('[00:00] You: So the budget is fine\n10:01 Agreed');
});

test('does nothing when Auto Summary is switched off', async () => {
  local.set('isAutoSummary', 'false');
  expect(await startPostCallSummary('m')).toEqual({ status: 'skipped', reason: 'disabled' });
  expect(calls).toHaveLength(0);
});

test('does not start a second run or replace an existing summary', async () => {
  handlers.api_get_summary = () => ({ status: 'processing', data: null });
  expect(await startPostCallSummary('m')).toEqual({ status: 'skipped', reason: 'running' });
  handlers.api_get_summary = () => ({ status: 'completed', data: { markdown: '# Notes' } });
  expect(await startPostCallSummary('m')).toEqual({ status: 'skipped', reason: 'exists' });
  expect(sent()).toHaveLength(0);
});

test('a re-run replaces a generated summary but never one the user edited', async () => {
  handlers.api_get_summary = () => ({ status: 'completed', data: { markdown: '# Notes' }, user_edited: false });
  expect((await startPostCallSummary('m', { replaceExisting: true })).status).toBe('started');
  handlers.api_get_summary = () => ({ status: 'completed', data: { markdown: '# Notes' }, user_edited: true });
  expect(await startPostCallSummary('m', { replaceExisting: true })).toEqual({ status: 'skipped', reason: 'exists' });
  expect(sent()).toHaveLength(1);
});

test('reports a backend that is already summarising this meeting as running', async () => {
  handlers.api_process_transcript = () => { throw new Error('Summary generation is already active for this meeting'); };
  expect(await startPostCallSummary('m')).toEqual({ status: 'skipped', reason: 'running' });
});

test('an empty transcript is skipped, not sent', async () => {
  handlers.api_get_meeting_transcripts = () => ({ transcripts: [], total_count: 0, has_more: false });
  expect(await startPostCallSummary('m')).toEqual({ status: 'skipped', reason: 'no-transcript' });
  expect(sent()).toHaveLength(0);
});

test('a local model that is downloading asks to be tried again; a missing one fails with the reason', async () => {
  handlers.api_get_model_config = () => ({ provider: 'builtin-ai', model: 'qwen' });
  handlers.builtin_ai_is_model_ready = () => false;
  handlers.builtin_ai_get_model_info = () => ({ status: { type: 'downloading', progress: 61 } });
  expect(await startPostCallSummary('m')).toEqual({
    status: 'waiting', message: 'The local summary model is still downloading (61%).',
  });
  handlers.builtin_ai_get_model_info = () => ({ status: { type: 'not_downloaded' } });
  const failed = await startPostCallSummary('m');
  expect(failed.status).toBe('failed');
  expect((failed as { message: string }).message).toContain('qwen is not downloaded');
  expect(sent()).toHaveLength(0);
});

test('no saved model and a refused request both fail with a reason instead of throwing', async () => {
  handlers.api_get_model_config = () => null;
  expect((await startPostCallSummary('m')).status).toBe('failed');
  handlers.api_get_model_config = () => ({ provider: 'claude', model: 'claude-opus-5-5' });
  handlers.api_process_transcript = () => ({});
  expect(await startPostCallSummary('m')).toEqual({ status: 'failed', message: 'The summary backend did not accept the request.' });
  handlers.api_process_transcript = () => { throw new Error('Failed to initialize process: disk full'); };
  expect(await startPostCallSummary('m')).toEqual({ status: 'failed', message: 'Failed to initialize process: disk full' });
});
