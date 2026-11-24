/**
 * Native record of the chain that runs after Stop (speaker question ->
 * re-transcription -> speaker labelling -> summary). The webview drives the
 * steps, but it can be reloaded or the app can quit mid-chain; the stage is
 * therefore stored natively and unfinished jobs are resumed on the next launch.
 */
import { invoke } from '@tauri-apps/api/core';

/** `summary` means: transcript and speakers are final, the summary is not started yet. */
export type PostCallStage = 'prompt' | 'enhancing' | 'diarizing' | 'summary' | 'done';

export interface PostCallJobRecord {
  meetingId: string;
  folderPath: string | null;
  stage: PostCallStage;
  /** False until the speaker question was answered; a resumed job asks again. */
  speakerChoiceMade: boolean;
  /** Total speakers including the user. `null` means auto-detect. */
  speakerCount: number | null;
  /** True when the large-model pass was skipped or failed and the live rows were kept. */
  usedLiveTranscript: boolean;
  /**
   * Why a step was skipped or downgraded, in words the meeting page can show.
   * In an update, an empty string clears the stored note; leaving it out keeps it.
   */
  note: string | null;
  /** Read-only: shown when the speaker question is asked again after a restart. */
  meetingTitle?: string | null;
}

export type PostCallJobUpdate = Pick<PostCallJobRecord, 'meetingId' | 'stage'>
  & Partial<Omit<PostCallJobRecord, 'meetingId' | 'stage' | 'meetingTitle'>>;

/** Dispatched on `window` after a stored stage change; `detail` is `{ meetingId, stage }`. */
export const POST_CALL_JOB_CHANGED_EVENT = 'meetily:post-call-job-changed';

/**
 * Persistence is best-effort: a failed write must never stop the chain itself.
 * Fields left out keep their stored value.
 */
export async function savePostCallJob(update: PostCallJobUpdate): Promise<void> {
  try {
    await invoke('post_call_job_save', { update });
  } catch (error) {
    console.warn('Could not store post-call progress:', error);
    return;
  }
  if (typeof window !== 'undefined' && typeof window.dispatchEvent === 'function' && typeof CustomEvent === 'function') {
    window.dispatchEvent(new CustomEvent(POST_CALL_JOB_CHANGED_EVENT, {
      detail: { meetingId: update.meetingId, stage: update.stage },
    }));
  }
}

export async function getPostCallJob(meetingId: string): Promise<PostCallJobRecord | null> {
  try {
    return (await invoke<PostCallJobRecord | null>('post_call_job_get', { meetingId })) ?? null;
  } catch {
    return null;
  }
}

export async function listUnfinishedPostCallJobs(): Promise<PostCallJobRecord[]> {
  try {
    return (await invoke<PostCallJobRecord[]>('post_call_jobs_unfinished')) ?? [];
  } catch {
    return [];
  }
}

/** Names typed at the speaker question, e.g. "Alice Smith, Bob". Order is kept. */
export function parseParticipantNames(input: string): string[] {
  const seen = new Set<string>();
  const names: string[] = [];
  for (const raw of input.split(/[,;\n]/)) {
    const name = raw.replace(/\s+/g, ' ').trim();
    const key = name.toLowerCase();
    if (!name || seen.has(key)) continue;
    seen.add(key);
    names.push(name);
  }
  return names;
}

export async function setMeetingParticipants(meetingId: string, names: string[]): Promise<void> {
  await invoke('meeting_participants_set', { meetingId, names });
}

export async function getMeetingParticipants(meetingId: string): Promise<string[]> {
  try {
    return (await invoke<string[]>('meeting_participants_get', { meetingId })) ?? [];
  } catch {
    return [];
  }
}

const isGenericRemoteLabel = (label: string) => /^(speaker(\s+\d+)?|guest)$/i.test(label.trim());

/**
 * A call with exactly one named guest and exactly one unnamed remote voice is
 * unambiguous: that voice is the guest. Anything else is left for the user,
 * because a model's speaker number is not an identity.
 */
export function singleGuestRename(names: string[], speakerLabels: string[]): { from: string; to: string } | null {
  if (names.length !== 1) return null;
  const components = new Set<string>();
  for (const label of speakerLabels) {
    for (const part of label.split(' + ')) {
      const trimmed = part.trim();
      if (trimmed) components.add(trimmed);
    }
  }
  const remote = [...components].filter((label) => !/^you$/i.test(label));
  if (remote.length !== 1 || !isGenericRemoteLabel(remote[0])) return null;
  return { from: remote[0], to: names[0] };
}
