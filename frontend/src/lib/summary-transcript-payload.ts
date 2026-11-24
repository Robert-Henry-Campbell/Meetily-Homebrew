/**
 * The text a summary is written from. One implementation is shared by the
 * meeting page and by the app-level post-call job, so both send the model the
 * same lines: `[mm:ss] Speaker: text`.
 */
import { invoke } from '@tauri-apps/api/core';
import type { Transcript } from '@/types';

export interface SummaryTranscriptPayload {
  transcriptText: string;
  transcriptTexts: string[];
}

function formatTime(seconds: number | undefined, fallbackTimestamp: string): string {
  if (seconds === undefined || seconds === null) return fallbackTimestamp;
  const totalSecs = Math.floor(seconds);
  const mins = Math.floor(totalSecs / 60);
  const secs = totalSecs % 60;
  return `[${mins.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}]`;
}

/** `clean` tidies each line before it is sent; the saved transcript is not changed. */
export function buildSummaryTranscriptPayload(
  transcripts: Transcript[],
  clean?: (text: string) => string,
): SummaryTranscriptPayload {
  const textOf = (t: Transcript) => (clean ? clean(t.text) || t.text : t.text);
  return {
    transcriptText: transcripts
      .map((t) => {
        const speaker = t.speaker ? `${t.speaker}: ` : '';
        return `${formatTime(t.audio_start_time, t.timestamp)} ${speaker}${textOf(t)}`;
      })
      .join('\n'),
    transcriptTexts: transcripts.map(textOf),
  };
}

interface TranscriptPage {
  transcripts: Transcript[];
  total_count: number;
  has_more: boolean;
}

/** Every saved line of the meeting, independent of what a page has loaded. */
export async function fetchAllTranscripts(meetingId: string): Promise<Transcript[]> {
  const firstPage = await invoke<TranscriptPage>('api_get_meeting_transcripts', {
    meetingId,
    limit: 1,
    offset: 0,
  });
  const totalCount = firstPage?.total_count ?? 0;
  if (totalCount === 0) return [];
  const all = await invoke<TranscriptPage>('api_get_meeting_transcripts', {
    meetingId,
    limit: totalCount,
    offset: 0,
  });
  return all?.transcripts ?? [];
}
