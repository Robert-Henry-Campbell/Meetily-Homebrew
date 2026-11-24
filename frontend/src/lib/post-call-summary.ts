/**
 * Starts the summary that follows a recording, without needing the meeting page.
 *
 * The app-level post-call job owner calls this when transcript and speaker
 * labels are final. The page used to start it, which lost the summary whenever
 * the user had navigated away or the app had restarted. The summary itself runs
 * natively after `api_process_transcript` accepts the request; an open meeting
 * page picks it up through SUMMARY_STARTED_EVENT.
 */
import { invoke } from '@tauri-apps/api/core';
import { cleanTranscriptText, loadLabsPreferences } from '@/lib/labs';
import { resolveSummaryLanguage } from '@/lib/summary-language-resolution';
import { buildSummaryTranscriptPayload, fetchAllTranscripts } from '@/lib/summary-transcript-payload';

export const SUMMARY_STARTED_EVENT = 'meetily:summary-started';
/** The meeting page's template picker starts on this template; an unattended run uses the same one. */
export const DEFAULT_SUMMARY_TEMPLATE = 'standard_meeting';

export type PostCallSummaryOutcome =
  | { status: 'started'; processId: string; provider: string; model: string }
  | { status: 'skipped'; reason: 'disabled' | 'exists' | 'running' | 'no-transcript' }
  /** The local summary model is still downloading; ask again later. */
  | { status: 'waiting'; message: string }
  | { status: 'failed'; message: string };

interface StoredModelConfig {
  provider?: string;
  model?: string;
}

interface StoredSummary {
  status?: string;
  data?: unknown;
  user_edited?: boolean;
}

interface BuiltInModelStatus {
  type?: string;
  progress?: number;
}

const ACTIVE_STATUSES = ['pending', 'processing', 'summarizing'];

/** Mirrors ConfigContext: Auto Summary is on unless the user switched it off. */
export function isAutoSummaryEnabled(): boolean {
  try {
    if (typeof localStorage === 'undefined') return true;
    const saved = localStorage.getItem('isAutoSummary');
    return saved !== null ? saved === 'true' : true;
  } catch {
    return true;
  }
}

function announceStarted(meetingId: string, processId: string): void {
  if (typeof window === 'undefined' || typeof window.dispatchEvent !== 'function') return;
  if (typeof CustomEvent !== 'function') return;
  window.dispatchEvent(new CustomEvent(SUMMARY_STARTED_EVENT, { detail: { meetingId, processId } }));
}

const describe = (cause: unknown) => (cause instanceof Error ? cause.message : String(cause));

export async function startPostCallSummary(
  meetingId: string,
  options: { replaceExisting?: boolean } = {},
): Promise<PostCallSummaryOutcome> {
  if (!isAutoSummaryEnabled()) return { status: 'skipped', reason: 'disabled' };

  try {
    const existing = await invoke<StoredSummary | null>('api_get_summary', { meetingId });
    const status = String(existing?.status ?? '').toLowerCase();
    if (ACTIVE_STATUSES.includes(status)) return { status: 'skipped', reason: 'running' };
    // A summary the user edited is never replaced without being asked.
    if (existing?.data && (!options.replaceExisting || existing.user_edited)) {
      return { status: 'skipped', reason: 'exists' };
    }
  } catch {
    // Status unknown. Starting is still safe: the backend refuses a second run.
  }

  let config: StoredModelConfig | null;
  try {
    config = await invoke<StoredModelConfig | null>('api_get_model_config');
  } catch (cause) {
    return { status: 'failed', message: `Could not read the summary model setting: ${describe(cause)}` };
  }
  const provider = config?.provider?.trim();
  const model = config?.model?.trim();
  if (!provider || !model) {
    return { status: 'failed', message: 'No summary model is selected. Choose one in Settings.' };
  }

  if (provider === 'builtin-ai') {
    try {
      const ready = await invoke<boolean>('builtin_ai_is_model_ready', { modelName: model, refresh: true });
      if (!ready) {
        const info = await invoke<{ status?: BuiltInModelStatus } | null>('builtin_ai_get_model_info', {
          modelName: model,
        }).catch(() => null);
        if (info?.status?.type === 'downloading') {
          return {
            status: 'waiting',
            message: `The local summary model is still downloading (${info.status.progress ?? 0}%).`,
          };
        }
        return {
          status: 'failed',
          message: `The local summary model ${model} is not downloaded. Download it in Settings.`,
        };
      }
    } catch (cause) {
      return { status: 'failed', message: `Could not check the local summary model: ${describe(cause)}` };
    }
  }

  let transcripts;
  try {
    transcripts = await fetchAllTranscripts(meetingId);
  } catch (cause) {
    return { status: 'failed', message: `Could not read the transcript: ${describe(cause)}` };
  }
  const labs = loadLabsPreferences();
  const payload = buildSummaryTranscriptPayload(
    transcripts,
    labs.cleanTranscript ? cleanTranscriptText : undefined,
  );
  if (!transcripts.length || !payload.transcriptText.trim()) {
    return { status: 'skipped', reason: 'no-transcript' };
  }

  const summaryLanguage = await resolveSummaryLanguage(meetingId, payload.transcriptTexts);

  try {
    const result = await invoke<{ process_id?: unknown }>('api_process_transcript', {
      text: payload.transcriptText,
      model: provider,
      modelName: model,
      meetingId,
      chunkSize: 40000,
      overlap: 1000,
      customPrompt: '',
      templateId: DEFAULT_SUMMARY_TEMPLATE,
      summaryLanguage,
    });
    const processId = result?.process_id;
    if (typeof processId !== 'string' || !processId.trim()) {
      return { status: 'failed', message: 'The summary backend did not accept the request.' };
    }
    announceStarted(meetingId, processId);
    return { status: 'started', processId, provider, model };
  } catch (cause) {
    const message = describe(cause);
    if (/already active/i.test(message)) return { status: 'skipped', reason: 'running' };
    return { status: 'failed', message };
  }
}
