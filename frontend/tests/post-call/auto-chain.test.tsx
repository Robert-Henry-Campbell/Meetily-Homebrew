/**
 * The chain after Stop: one question, then no further click. Covered here:
 * names typed at the question, the exact post-call model (waited for, never
 * replaced), the no-progress timer, the automatic error path, resume after a
 * restart, and the summary that starts without the meeting page.
 */
import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';

const storage = new Map<string, string>();
const events = new Map<string, (event: any) => void>();
const calls: Array<[string, any]> = [];
const toasts: Array<[string, string, any]> = [];
let handlers: Record<string, (args: any) => unknown>;
Object.assign(globalThis, { React,
  sessionStorage: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    removeItem: (key: string) => storage.delete(key),
  },
  window: new EventTarget(),
});
mock.module('@tauri-apps/api/event', () => ({ listen: async (name: string, fn: any) => {
  events.set(name, fn); return () => events.delete(name);
} }));
mock.module('@tauri-apps/api/core', () => ({ invoke: async (name: string, args: any) => {
  calls.push([name, args]);
  return handlers[name]?.(args);
} }));
mock.module('@/hooks/useDiarizationEngine', () => ({ useDiarizationEngine: () => ({ engine: 'pyannote', isNemotron: false }) }));
mock.module('@/contexts/ConfigContext', () => ({ useConfig: () => ({
  selectedLanguage: 'en',
  transcriptModelConfig: { provider: 'parakeet', model: 'parakeet-live' },
}) }));
mock.module('@/lib/parakeet', () => ({ isVisibleParakeetModel: () => true }));
mock.module('@/lib/workspace-api', () => ({ announceChange: () => {} }));
mock.module('@/lib/summary-language-resolution', () => ({ resolveSummaryLanguage: async () => null }));
const toast = (kind: string) => (title: string, options?: any) => { toasts.push([kind, title, options]); };
mock.module('sonner', () => ({ toast: { success: toast('success'), error: toast('error'), info: toast('info'), warning: toast('warning') } }));
mock.module('@/components/PostCallHandoffCard', () => ({ PostCallHandoffCard: ({ children, title, detail }: any) => <section data-title={title}>{detail}{children}</section> }));
mock.module('@/components/ui/button', () => ({ Button: (props: any) => <button {...props} /> }));
mock.module('next/link', () => ({ default: ({ children, href }: any) => <a href={href}>{children}</a> }));

const { PostCallJobsProvider, postCallOwnerTimings } = await import('../../src/contexts/PostCallJobsContext');
const { PostCallNotice } = await import('../../src/components/MeetingDetails/PostCallNotice');
const { PostCallProcessingDialog } = await import('../../src/components/MeetingDetails/PostCallProcessingDialog');
const { postCallTimings } = await import('../../src/components/MeetingDetails/PostCallProcessingWorker');
const defaults = { ...postCallTimings };
const ownerDefaults = { ...postCallOwnerTimings };

let root: ReactTestRenderer;
const refresh = mock(async () => {});
function App({ page = 'a' }: { page?: string }) {
  return <PostCallJobsProvider>{page !== 'settings' && <PostCallProcessingDialog
    enabled meetingId={page} meetingFolderPath={`/${page}`}
    onRefetchTranscripts={refresh} onComplete={() => {}} />}</PostCallJobsProvider>;
}
const button = (label: string) => root.root.findAllByType('button').find(node => node.children.includes(label));
const named = (name: string) => calls.filter(([command]) => command === name);
const saves = () => named('post_call_job_save').map(([, args]) => args.update);
const wait = (ms: number) => act(async () => { await new Promise(resolve => setTimeout(resolve, ms)); });
async function until(condition: () => boolean, label: string) {
  for (let i = 0; i < 200; i += 1) {
    if (condition()) return;
    await wait(5);
  }
  throw new Error(`Timed out waiting for: ${label}`);
}
const emit = (name: string, payload: any) => act(async () => events.get(name)!({ payload }));

/** A machine whose configured post-call model is present. */
function readyMachine(overrides: Record<string, (args: any) => unknown> = {}) {
  handlers = {
    api_get_post_call_transcript_config: () => ({ provider: 'whisper', model: 'large-v3' }),
    whisper_get_available_models: () => [{ name: 'tiny', status: 'Available' }, { name: 'large-v3', status: 'Available' }],
    parakeet_get_available_models: () => [{ name: 'parakeet-live', status: 'Available' }],
    is_retranscription_in_progress_command: () => false,
    ...overrides,
  };
}

beforeEach(() => {
  storage.clear(); calls.length = 0; toasts.length = 0; refresh.mockClear();
  Object.assign(postCallTimings, defaults, { retryDelayMs: 5, pollMs: 5 });
  // Launch-time resume is exercised by its own tests.
  Object.assign(postCallOwnerTimings, ownerDefaults, { resumeDelayMs: 60000 });
  readyMachine();
});
afterEach(async () => { await act(async () => root?.unmount()); events.clear(); });

test('names typed at the question are stored, and a single guest receives the name', async () => {
  let participants: string[] = [];
  readyMachine({
    meeting_participants_set: ({ names }) => { participants = names; return names; },
    meeting_participants_get: () => participants,
    meeting_speaker_labels: () => ['You', 'Speaker 1'],
  });
  await act(async () => { root = create(<App />); });
  const field = root.root.findByProps({ 'aria-label': 'Names of the other people on the call' });
  await act(async () => field.props.onChange({ target: { value: ' Alice  Smith ,' } }));
  await act(async () => button('Continue')!.props.onClick());

  expect(named('meeting_participants_set')[0][1]).toEqual({ meetingId: 'a', names: ['Alice Smith'] });
  expect(saves().find(update => update.stage === 'enhancing')).toMatchObject({
    meetingId: 'a', folderPath: '/a', speakerChoiceMade: true, speakerCount: 2,
  });
  expect(named('start_retranscription_command')[0][1]).toMatchObject({ meetingId: 'a', model: 'large-v3', provider: 'whisper' });

  await emit('retranscription-complete', { meeting_id: 'a' });
  expect(named('diarize_meeting')[0][1]).toEqual({ meetingId: 'a', numSpeakers: 2 });
  expect(named('rename_meeting_speaker')[0][1]).toEqual({ meetingId: 'a', from: 'Speaker 1', to: 'Alice Smith' });
  expect(saves().find(update => update.stage === 'summary')).toMatchObject({ usedLiveTranscript: false, note: '' });
});

test('several names are never assigned to speaker numbers', async () => {
  readyMachine({
    meeting_participants_get: () => ['Alice', 'Bob'],
    meeting_speaker_labels: () => ['You', 'Speaker 1', 'Speaker 2'],
  });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await emit('retranscription-complete', { meeting_id: 'a' });
  expect(named('diarize_meeting')).toHaveLength(1);
  expect(named('rename_meeting_speaker')).toHaveLength(0);
});

test('names already known for the meeting are offered in the question', async () => {
  // The calendar event of the recording is looked up first; its invitees are offered.
  readyMachine({ calendar_match_meeting: () => ({ status: 'matched' }), meeting_suggested_participants: () => ['Dana Lee'] });
  await act(async () => { root = create(<App />); });
  await wait(5);
  expect(root.root.findByProps({ 'aria-label': 'Names of the other people on the call' }).props.value).toBe('Dana Lee');
  const order = calls.map(([name]) => name).filter(name => name.startsWith('calendar_') || name === 'meeting_suggested_participants');
  expect(order).toEqual(['calendar_match_meeting', 'meeting_suggested_participants']);
  // Continuing with the offered names stores them, so they also name the Drive folder.
  await act(async () => button('Continue')!.props.onClick());
  expect(named('meeting_participants_set')[0][1]).toEqual({ meetingId: 'a', names: ['Dana Lee'] });
});

test('a calendar lookup that fails does not hold up the question', async () => {
  readyMachine({ calendar_match_meeting: () => { throw new Error('Google Calendar could not be reached.'); } });
  await act(async () => { root = create(<App />); });
  await wait(5);
  expect(root.root.findByProps({ 'aria-label': 'Names of the other people on the call' }).props.value).toBe('');
  await act(async () => button('Continue')!.props.onClick());
  expect(named('start_retranscription_command')).toHaveLength(1);
});

test('a missing post-call model is not replaced by a smaller one; the live transcript is kept and marked', async () => {
  readyMachine({
    whisper_get_available_models: () => [{ name: 'tiny', status: 'Available' }, { name: 'large-v3', status: 'Missing' }],
  });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());

  expect(named('start_retranscription_command')).toHaveLength(0);
  expect(named('diarize_meeting')).toHaveLength(1);
  const final = saves().find(update => update.stage === 'summary')!;
  expect(final.usedLiveTranscript).toBe(true);
  expect(final.note).toContain('large-v3 is not downloaded');
  expect(toasts.some(([kind, title]) => kind === 'warning' && title === 'Kept the live transcript')).toBe(true);
  // The chain finished on its own: no error dialog waits for a click.
  expect(root.root.findAllByType('section')).toHaveLength(0);
});

test('the chain waits while the post-call model downloads, then uses exactly that model', async () => {
  let status: unknown = { Downloading: { progress: 40 } };
  readyMachine({ whisper_get_available_models: () => [{ name: 'tiny', status: 'Available' }, { name: 'large-v3', status }] });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await wait(30);
  expect(named('start_retranscription_command')).toHaveLength(0);
  expect(root.root.findByType('section').children.join('')).toContain('Waiting for the large-v3 download (40%)');

  status = 'Available';
  await until(() => named('start_retranscription_command').length === 1, 'start after download');
  expect(named('start_retranscription_command')[0][1]).toMatchObject({ model: 'large-v3', provider: 'whisper' });
});

test('a recording that ends during the setup download waits for the large model instead of the live one', async () => {
  let config = { provider: 'live', model: '' };
  let status: unknown = { Downloading: { progress: 7 } };
  readyMachine({
    api_get_post_call_transcript_config: () => config,
    whisper_get_available_models: () => [{ name: 'large-v3', status }],
  });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await wait(30);
  expect(named('start_retranscription_command')).toHaveLength(0);

  // Native activation switches the post-call setting when the download completes.
  status = 'Available';
  config = { provider: 'whisper', model: 'large-v3' };
  await until(() => named('start_retranscription_command').length === 1, 'start with the large model');
  expect(named('start_retranscription_command')[0][1]).toMatchObject({ model: 'large-v3', provider: 'whisper' });
});

test('a "live" post-call setting without a download in progress uses the live model as configured', async () => {
  readyMachine({ api_get_post_call_transcript_config: () => ({ provider: 'live', model: '' }) });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  expect(named('start_retranscription_command')[0][1]).toMatchObject({ model: 'parakeet-live', provider: 'parakeet', language: null });
});

test('a failed pass is retried once; a second failure keeps the live transcript and the chain continues', async () => {
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await emit('retranscription-error', { meeting_id: 'a', error: 'GPU ran out of memory' });
  await until(() => named('start_retranscription_command').length === 2, 'automatic retry');
  expect(named('diarize_meeting')).toHaveLength(0);

  await emit('retranscription-error', { meeting_id: 'a', error: 'GPU ran out of memory' });
  expect(named('start_retranscription_command')).toHaveLength(2);
  expect(named('diarize_meeting')).toHaveLength(1);
  const final = saves().find(update => update.stage === 'summary')!;
  expect(final).toMatchObject({ usedLiveTranscript: true });
  expect(final.note).toContain('GPU ran out of memory');
  expect(button('Retry')).toBeUndefined();
});

test('a retry that succeeds leaves no mark on the meeting', async () => {
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await emit('retranscription-error', { meeting_id: 'a', error: 'temporary' });
  await until(() => named('start_retranscription_command').length === 2, 'automatic retry');
  await emit('retranscription-complete', { meeting_id: 'a' });
  expect(saves().find(update => update.stage === 'summary')).toMatchObject({ usedLiveTranscript: false, note: '' });
  expect(toasts.some(([kind]) => kind === 'warning')).toBe(false);
});

test('a recording without speech is not retried', async () => {
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await emit('retranscription-error', { meeting_id: 'a', error: 'No speech detected in audio file' });
  expect(named('start_retranscription_command')).toHaveLength(1);
  expect(named('diarize_meeting')).toHaveLength(1);
});

test('failed speaker labelling is retried once and then reported without blocking the chain', async () => {
  readyMachine({ diarize_meeting: () => { throw new Error('Speaker model is missing'); } });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await emit('retranscription-complete', { meeting_id: 'a' });
  await until(() => saves().some(update => update.stage === 'summary'), 'chain completion');
  expect(named('diarize_meeting')).toHaveLength(2);
  const final = saves().find(update => update.stage === 'summary')!;
  expect(final.usedLiveTranscript).toBe(false);
  expect(final.note).toContain('Speakers were not labelled. Speaker model is missing');
});

test('progress keeps a long pass alive; silence past the limit cancels it', async () => {
  postCallTimings.stallMs = 120;
  let active = true;
  readyMachine({
    is_retranscription_in_progress_command: ({ meetingId }) => meetingId === 'a' && active && named('cancel_retranscription_command').length > 0,
    cancel_retranscription_command: () => { active = false; },
  });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  // Well past the limit in total, but never silent for that long.
  for (let i = 0; i < 12; i += 1) {
    await wait(30);
    await emit('retranscription-progress', { meeting_id: 'a', progress_percentage: 30, message: 'Transcribing segment...' });
  }
  expect(named('cancel_retranscription_command')).toHaveLength(0);

  await until(() => named('cancel_retranscription_command').length === 1, 'cancel after silence');
  expect(named('cancel_retranscription_command')[0][1]).toEqual({ meetingId: 'a' });
  // The stalled attempt counts as a failure, so the single retry starts.
  await until(() => named('start_retranscription_command').length === 2, 'retry after the stall');
});

test('a pass of another meeting is waited for, not treated as a failure', async () => {
  let busy = true;
  readyMachine({ is_retranscription_in_progress_command: ({ meetingId }) => meetingId === null && busy });
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await wait(30);
  expect(named('start_retranscription_command')).toHaveLength(0);
  busy = false;
  await until(() => named('start_retranscription_command').length === 1, 'start after the other pass');
});

test('Cancel still hands the decision back to the user', async () => {
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await act(async () => button('Cancel processing')!.props.onClick());
  await emit('retranscription-error', { meeting_id: 'a', error: 'Retranscription cancelled' });
  await wait(20);
  expect(named('start_retranscription_command')).toHaveLength(1);
  expect(named('diarize_meeting')).toHaveLength(0);
  expect(button('Retry')).toBeDefined();
  expect(button('Keep live transcript')).toBeDefined();
});

const transcriptRows = [
  { id: 't1', text: 'hello there', timestamp: '10:00', audio_start_time: 1.4, speaker: 'You' },
  { id: 't2', text: 'hi', timestamp: '10:00', audio_start_time: 65, speaker: 'Alice' },
];
const summaryMachine = (overrides: Record<string, (args: any) => unknown> = {}) => readyMachine({
  api_get_summary: () => ({ status: 'idle', data: null }),
  api_get_model_config: () => ({ provider: 'claude', model: 'claude-opus-5-5' }),
  api_get_meeting_transcripts: () => ({ transcripts: transcriptRows, total_count: 2, has_more: false }),
  api_process_transcript: () => ({ process_id: 'p1' }),
  ...overrides,
});

test('the summary starts when the chain ends, with the meeting page closed', async () => {
  summaryMachine();
  await act(async () => { root = create(<App />); });
  await act(async () => button('Continue')!.props.onClick());
  await act(async () => root.update(<App page="settings" />));
  await emit('retranscription-complete', { meeting_id: 'a' });
  await until(() => named('api_process_transcript').length === 1, 'summary start');

  expect(named('api_process_transcript')[0][1]).toMatchObject({
    meetingId: 'a',
    model: 'claude',
    modelName: 'claude-opus-5-5',
    templateId: 'standard_meeting',
    text: '[00:01] You: hello there\n[01:05] Alice: hi',
  });
  await until(() => saves().some(update => update.stage === 'done'), 'job marked done');
  // Order matters to the export queue: final transcript first, then the summary request.
  const stages = saves().map(update => update.stage);
  expect(stages.indexOf('summary')).toBeLessThan(stages.indexOf('done'));
});

test('an interrupted job resumes after a restart without asking again', async () => {
  summaryMachine({
    post_call_jobs_unfinished: () => [{
      meetingId: 'm', folderPath: '/recordings/m', stage: 'enhancing', speakerChoiceMade: true,
      speakerCount: 3, usedLiveTranscript: false, note: null, meetingTitle: 'Weekly sync',
    }],
  });
  postCallOwnerTimings.resumeDelayMs = 0;
  await act(async () => { root = create(<App page="settings" />); });
  await until(() => named('start_retranscription_command').length === 1, 'resumed pass');
  expect(named('start_retranscription_command')[0][1]).toMatchObject({ meetingId: 'm', meetingFolderPath: '/recordings/m' });
  expect(button('Continue')).toBeUndefined();

  await emit('retranscription-complete', { meeting_id: 'm' });
  await until(() => named('api_process_transcript').length === 1, 'summary after resume');
  expect(named('diarize_meeting')[0][1]).toEqual({ meetingId: 'm', numSpeakers: 3 });
});

test('a job interrupted during speaker labelling does not repeat the re-transcription', async () => {
  summaryMachine({
    post_call_jobs_unfinished: () => [{
      meetingId: 'm', folderPath: '/recordings/m', stage: 'diarizing', speakerChoiceMade: true,
      speakerCount: null, usedLiveTranscript: true, note: 'The live transcript was kept.', meetingTitle: 'Weekly sync',
    }],
  });
  postCallOwnerTimings.resumeDelayMs = 0;
  await act(async () => { root = create(<App page="settings" />); });
  await until(() => named('api_process_transcript').length === 1, 'summary after resume');
  expect(named('start_retranscription_command')).toHaveLength(0);
  expect(named('diarize_meeting')[0][1]).toEqual({ meetingId: 'm', numSpeakers: null });
  // What happened before the restart is still on record.
  expect(saves().find(update => update.stage === 'summary')).toMatchObject({
    usedLiveTranscript: true, note: 'The live transcript was kept.',
  });
});

test('a question that was never answered is asked again after a restart, naming the meeting', async () => {
  readyMachine({
    post_call_jobs_unfinished: () => [{
      meetingId: 'm', folderPath: '/recordings/m', stage: 'prompt', speakerChoiceMade: false,
      speakerCount: null, usedLiveTranscript: false, note: null, meetingTitle: 'Weekly sync',
    }],
  });
  postCallOwnerTimings.resumeDelayMs = 0;
  await act(async () => { root = create(<App page="settings" />); });
  await until(() => button('Continue') !== undefined, 'question shown');
  expect(named('start_retranscription_command')).toHaveLength(0);
  expect(JSON.stringify(root.toJSON())).toContain('Weekly sync');
});

test('a job that only lacks its summary starts the summary at launch and nothing else', async () => {
  summaryMachine({
    post_call_jobs_unfinished: () => [{
      meetingId: 'm', folderPath: '/recordings/m', stage: 'summary', speakerChoiceMade: true,
      speakerCount: 2, usedLiveTranscript: false, note: null, meetingTitle: 'Weekly sync',
    }],
  });
  postCallOwnerTimings.resumeDelayMs = 0;
  await act(async () => { root = create(<App page="settings" />); });
  await until(() => saves().some(update => update.stage === 'done'), 'job marked done');
  expect(named('api_process_transcript')).toHaveLength(1);
  expect(named('start_retranscription_command')).toHaveLength(0);
  expect(named('diarize_meeting')).toHaveLength(0);
});

test('the summary waits for a local model that is still downloading', async () => {
  let ready = false;
  summaryMachine({
    api_get_model_config: () => ({ provider: 'builtin-ai', model: 'qwen' }),
    builtin_ai_is_model_ready: () => ready,
    builtin_ai_get_model_info: () => ({ status: { type: 'downloading', progress: 12 } }),
    post_call_jobs_unfinished: () => [{
      meetingId: 'm', folderPath: '/recordings/m', stage: 'summary', speakerChoiceMade: true,
      speakerCount: 2, usedLiveTranscript: false, note: null,
    }],
  });
  Object.assign(postCallOwnerTimings, { resumeDelayMs: 0, summaryRetryMs: 5 });
  await act(async () => { root = create(<App page="settings" />); });
  await wait(40);
  expect(named('api_process_transcript')).toHaveLength(0);
  expect(saves().some(update => update.stage === 'done')).toBe(false);
  ready = true;
  await until(() => named('api_process_transcript').length === 1, 'summary once the model is ready');
  expect(toasts.filter(([, title]) => title === 'The summary starts when its model is ready')).toHaveLength(1);
});

test('the meeting says what was skipped, and "Run again" repeats the chain with the stored answer', async () => {
  const stored = {
    meetingId: 'a', folderPath: '/a', stage: 'done', speakerChoiceMade: true, speakerCount: 4,
    usedLiveTranscript: true, note: 'The accurate re-transcription did not finish, so the live transcript was kept.',
  };
  summaryMachine({
    post_call_job_get: () => stored,
    // A generated summary of the old transcript exists and was not edited.
    api_get_summary: () => ({ status: 'completed', data: { markdown: 'old' }, user_edited: false }),
  });
  await act(async () => {
    root = create(<PostCallJobsProvider><PostCallNotice meetingId="a" meetingFolderPath="/a" /></PostCallJobsProvider>);
  });
  expect(JSON.stringify(root.toJSON())).toContain('the live transcript was kept');

  await act(async () => button('Run again')!.props.onClick());
  await until(() => named('start_retranscription_command').length === 1, 'second run');
  expect(button('Continue')).toBeUndefined();
  await emit('retranscription-complete', { meeting_id: 'a' });
  await until(() => named('api_process_transcript').length === 1, 'summary of the new transcript');
  expect(named('diarize_meeting')[0][1]).toEqual({ meetingId: 'a', numSpeakers: 4 });
  // The successful run clears the mark.
  expect(saves().find(update => update.stage === 'summary')).toMatchObject({ usedLiveTranscript: false, note: '' });
});

test('a meeting whose chain finished cleanly, or is still running, shows no notice', async () => {
  let stored: any = { meetingId: 'a', stage: 'done', speakerChoiceMade: true, usedLiveTranscript: false, note: null };
  readyMachine({ post_call_job_get: () => stored });
  await act(async () => { root = create(<PostCallJobsProvider><PostCallNotice meetingId="a" /></PostCallJobsProvider>); });
  expect(root.toJSON()).toBeNull();
  await act(async () => root.unmount());

  stored = { meetingId: 'a', stage: 'diarizing', speakerChoiceMade: true, usedLiveTranscript: true, note: 'kept' };
  await act(async () => { root = create(<PostCallJobsProvider><PostCallNotice meetingId="a" /></PostCallJobsProvider>); });
  expect(root.toJSON()).toBeNull();
});
