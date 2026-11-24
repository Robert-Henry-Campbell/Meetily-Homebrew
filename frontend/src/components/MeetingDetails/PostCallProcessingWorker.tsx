"use client";

/**
 * Owns the post-recording handoff. This must remain a single ordered workflow:
 * speaker question -> source-track retranscription -> refetch -> diarization ->
 * refetch -> summary gate. Starting summary or diarization elsewhere races the
 * transactional transcript replacement.
 *
 * The speaker question is the only step that waits for the user. After it, the
 * chain runs unattended: a failed step is retried once, and if it fails again
 * the chain continues with what it has (the live transcript, or unlabelled
 * speakers) and records why. Only an explicit Cancel returns to a manual choice.
 * Each stage change is stored natively so a reload or a quit can resume it.
 *
 * Retranscription is event-driven because the Tauri start command returns after
 * spawning its native task. Listeners therefore register before invoke and are
 * meeting-ID filtered. Audio-disabled meetings can explicitly continue with
 * their live transcript so the summary stage is never permanently blocked.
 */

import { useEffect, useRef, useState } from 'react';
import Link from 'next/link';
import { invoke } from '@tauri-apps/api/core';
import { useDiarizationEngine } from '@/hooks/useDiarizationEngine';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

import { toast } from 'sonner';
import { useConfig } from '@/contexts/ConfigContext';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { fieldClass } from '@/components/ui/input';
import { PostCallHandoffCard } from '@/components/PostCallHandoffCard';
import type { RawModelInfo } from '@/hooks/useTranscriptionModels';
import { isVisibleParakeetModel } from '@/lib/parakeet';
import { announceChange } from '@/lib/workspace-api';
import { OPTIONAL_WHISPER_MODEL } from '@/lib/optional-model-activation';
import {
  getMeetingParticipants,
  getSuggestedParticipants,
  parseParticipantNames,
  savePostCallJob,
  setMeetingParticipants,
  singleGuestRename,
  type PostCallJobRecord,
} from '@/lib/post-call-jobs';

type Stage = 'idle' | 'prompt' | 'enhancing' | 'diarizing' | 'refreshing' | 'error';
type FailedStage = 'enhancing' | 'diarizing';

/** Waits and intervals of the unattended chain. Tests shorten them. */
export const postCallTimings = {
  /** Cancel a re-transcription that reports no progress for this long. */
  stallMs: 20 * 60 * 1000,
  /** Pause before the single automatic retry of a failed step. */
  retryDelayMs: 3000,
  /** How often a model download or a busy engine is checked again. */
  pollMs: 3000,
};

interface RetranscriptionProgress {
  meeting_id: string;
  progress_percentage: number;
  message: string;
}

interface RetranscriptionResult {
  meeting_id: string;
}

interface RetranscriptionError {
  meeting_id: string;
  error: string;
}

interface ModelChoice {
  provider: 'whisper' | 'parakeet';
  name: string;
}

interface PostCallTranscriptConfig {
  provider: 'live' | 'whisper' | 'parakeet';
  model: string;
}

const CANCELLED = 'Post-call processing cancelled.';

/** A failure that a second attempt cannot fix (missing model, missing audio). */
class PermanentStepError extends Error {}

const describe = (cause: unknown) => (cause instanceof Error ? cause.message : String(cause));

/** Native failures that are a property of the recording, not of this attempt. */
const isPermanentNativeFailure = (message: string) =>
  /no speech detected|no audio file|audio file not found|recording folder/i.test(message);

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new Error(CANCELLED));
      return;
    }
    const onAbort = () => {
      clearTimeout(timer);
      reject(new Error(CANCELLED));
    };
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    signal.addEventListener('abort', onAbort, { once: true });
  });
}

function downloadPercent(status: RawModelInfo['status'] | undefined): number | null {
  if (!status || typeof status !== 'object' || !('Downloading' in status)) return null;
  const value = (status as { Downloading: unknown }).Downloading;
  if (typeof value === 'number') return value;
  const progress = (value as { progress?: unknown } | null)?.progress;
  return typeof progress === 'number' ? progress : 0;
}

/**
 * Chooses the model for the accurate pass after Stop.
 *
 * A configured post-call model is used exactly: while it downloads the chain
 * waits, and if it is absent the step fails with the reason. It is never
 * replaced by whichever smaller model happens to be on disk, because the result
 * would look like the accurate transcript without being one.
 */
async function resolveEnhancementModel({
  liveProvider,
  liveModel,
  signal,
  onWaiting,
}: {
  liveProvider?: string;
  liveModel?: string;
  signal: AbortSignal;
  onWaiting: (message: string) => void;
}): Promise<ModelChoice> {
  let lastPercent = -1;
  let lastChange = Date.now();
  /** True while a download is still moving; false once it has stalled. */
  const stillArriving = (percent: number) => {
    if (percent !== lastPercent) {
      lastPercent = percent;
      lastChange = Date.now();
    }
    return Date.now() - lastChange < postCallTimings.stallMs;
  };

  while (true) {
    if (signal.aborted) throw new Error(CANCELLED);
    const postCall = await invoke<PostCallTranscriptConfig>('api_get_post_call_transcript_config')
      .catch(() => null) ?? { provider: 'live' as const, model: '' };
    const [whisperModels, parakeetModels] = await Promise.all([
      invoke<RawModelInfo[]>('whisper_get_available_models').catch(() => []),
      invoke<RawModelInfo[]>('parakeet_get_available_models').catch(() => []),
    ]).then((lists) => lists.map((list) => list ?? []));

    if (postCall.provider === 'whisper' || postCall.provider === 'parakeet') {
      const entry = (postCall.provider === 'whisper' ? whisperModels : parakeetModels)
        .find((model) => model.name === postCall.model);
      if (entry?.status === 'Available') return { provider: postCall.provider, name: postCall.model };
      const percent = downloadPercent(entry?.status);
      if (percent !== null && stillArriving(percent)) {
        onWaiting(`Waiting for the ${postCall.model} download (${percent}%)...`);
        await sleep(postCallTimings.pollMs, signal);
        continue;
      }
      throw new PermanentStepError(
        percent !== null
          ? `The download of the post-call model ${postCall.model} has stalled.`
          : `The post-call model ${postCall.model} is not downloaded. Download it in Settings, or choose another post-call model.`,
      );
    }

    // "live" means the second pass reuses the live model. Setup downloads the
    // large model in the background and native activation switches the
    // post-call setting to it on completion, so a recording that ends during
    // that download waits for it instead of settling for the live model.
    const optional = whisperModels.find((model) => model.name === OPTIONAL_WHISPER_MODEL);
    const optionalPercent = downloadPercent(optional?.status);
    if (optionalPercent !== null && stillArriving(optionalPercent)) {
      onWaiting(`Waiting for the ${OPTIONAL_WHISPER_MODEL} download (${optionalPercent}%)...`);
      await sleep(postCallTimings.pollMs, signal);
      continue;
    }

    const available: ModelChoice[] = [
      ...parakeetModels
        .filter((model) => model.status === 'Available' && isVisibleParakeetModel(model.name))
        .map((model) => ({ provider: 'parakeet' as const, name: model.name })),
      ...whisperModels
        .filter((model) => model.status === 'Available')
        .map((model) => ({ provider: 'whisper' as const, name: model.name })),
    ];
    const normalizedProvider = liveProvider === 'localWhisper' ? 'whisper' : liveProvider;
    const configured = available.find(
      (model) => model.provider === normalizedProvider && model.name === liveModel,
    );
    if (configured) return configured;
    if (normalizedProvider === 'whisper' || normalizedProvider === 'parakeet') {
      const sameProvider = available.find((model) => model.provider === normalizedProvider);
      if (sameProvider) return sameProvider;
      throw new PermanentStepError(`No downloaded ${normalizedProvider} model is available for enhancement.`);
    }
    const localDefault = available.find((model) => model.provider === 'parakeet')
      ?? available.find((model) => model.provider === 'whisper')
      ?? available[0];
    if (localDefault) return localDefault;
    throw new PermanentStepError('No downloaded transcription model is available for post-call enhancement.');
  }
}

async function runRetranscription({
  meetingId,
  meetingFolderPath,
  language,
  model,
  onProgress,
  signal,
}: {
  meetingId: string;
  meetingFolderPath: string;
  language: string | null;
  model: ModelChoice;
  onProgress: (progress: RetranscriptionProgress) => void;
  signal: AbortSignal;
}): Promise<void> {
  const unlisteners: UnlistenFn[] = [];
  let settled = false;
  let timedOut = false;
  let lastActivity = Date.now();
  let resolveCompletion!: () => void;
  let rejectCompletion!: (error: unknown) => void;
  let watchdog: ReturnType<typeof setInterval> | undefined;
  const completion = new Promise<void>((resolve, reject) => {
    resolveCompletion = resolve;
    rejectCompletion = reject;
  });
  const cleanup = () => {
    signal.removeEventListener('abort', cancel);
    if (watchdog) clearInterval(watchdog);
    unlisteners.splice(0).forEach((unlisten) => unlisten());
  };

  const cancel = () => {
    // Completion/error still owns settlement. Cancelling a JS promise alone
    // must not release the job while native inference is still using models.
    void invoke('cancel_retranscription_command', { meetingId }).catch(() => undefined);
  };
  const finish = (error?: unknown) => {
    if (settled) return;
    settled = true;
    cleanup();
    if (error) rejectCompletion(error);
    else resolveCompletion();
  };

  try {
    unlisteners.push(await listen<RetranscriptionProgress>(
      'retranscription-progress',
      (event) => {
        if (event.payload.meeting_id !== meetingId) return;
        lastActivity = Date.now();
        onProgress(event.payload);
      },
    ));
    unlisteners.push(await listen<RetranscriptionResult>(
      'retranscription-complete',
      (event) => {
        if (!timedOut && event.payload.meeting_id === meetingId) finish();
      },
    ));
    unlisteners.push(await listen<RetranscriptionError>(
      'retranscription-error',
      (event) => {
        if (!timedOut && event.payload.meeting_id === meetingId) {
          finish(new Error(event.payload.error));
        }
      },
    ));

    try {
      if (signal.aborted) throw new Error(CANCELLED);
      // After a reload the native pass of this meeting can still be running.
      // Its events arrive on the listeners above; a second start would be refused.
      const alreadyRunning = await invoke<boolean>('is_retranscription_in_progress_command', { meetingId })
        .catch(() => false);
      if (alreadyRunning !== true) {
        // The native engine runs one pass at a time. A pass of another meeting
        // (started from its page) is waited for, not treated as a failure.
        while (await invoke<boolean>('is_retranscription_in_progress_command', { meetingId: null })
          .catch(() => false) === true) {
          onProgress({
            meeting_id: meetingId,
            progress_percentage: 0,
            message: 'Waiting for another transcription to finish...',
          });
          await sleep(postCallTimings.pollMs, signal);
        }
        await invoke('start_retranscription_command', {
          meetingId,
          meetingFolderPath,
          language,
          model: model.name,
          provider: model.provider,
          vocabularyTerms: null,
          vocabularyScope: null,
        });
      }
      lastActivity = Date.now();
      signal.addEventListener('abort', cancel);
      if (signal.aborted) cancel();
    } catch (error) {
      finish(error);
    }
    // A fixed limit cancelled long recordings on slow graphics. Progress
    // arrives before every segment (at most 25 s of audio), so silence for this
    // long means the pass is stuck, whatever the length of the recording.
    watchdog = setInterval(() => {
      if (settled || timedOut || Date.now() - lastActivity < postCallTimings.stallMs) return;
      timedOut = true;
      void (async () => {
        await invoke('cancel_retranscription_command', { meetingId }).catch(() => undefined);
        // Keep ownership if inference outlives the timeout or status IPC fails.
        // A timer cannot prove that native resources have been released.
        while (true) {
          const active = await invoke<boolean>('is_retranscription_in_progress_command', { meetingId })
            .catch(() => true);
          if (!active) break;
          await new Promise((resolve) => setTimeout(resolve, 2000));
        }
        const minutes = Math.max(1, Math.round(postCallTimings.stallMs / 60000));
        finish(new Error(`The re-transcription made no progress for ${minutes} minutes and was cancelled.`));
      })();
    }, Math.max(250, Math.min(30000, Math.floor(postCallTimings.stallMs / 4))));
    await completion;
  } finally {
    cleanup();
  }
}

export function PostCallProcessingWorker({
  enabled,
  meetingId,
  meetingFolderPath,
  resume,
  onRefetchTranscripts,
  onComplete,
}: {
  enabled: boolean;
  meetingId: string;
  meetingFolderPath?: string | null;
  /** Stored progress of a job that was interrupted by a reload or a quit. */
  resume?: PostCallJobRecord | null;
  onRefetchTranscripts?: () => Promise<void>;
  onComplete: () => void;
}) {
  const { selectedLanguage, transcriptModelConfig } = useConfig();
  // An interrupted job whose speaker question was answered continues without asking again.
  const resumeFrom: FailedStage | null = resume?.speakerChoiceMade
    ? resume.stage === 'diarizing' ? 'diarizing' : resume.stage === 'enhancing' ? 'enhancing' : null
    : null;
  const [stage, setStage] = useState<Stage>(enabled ? (resumeFrom ?? 'prompt') : 'idle');
  const { engine, isNemotron, error: engineError } = useDiarizationEngine(stage !== 'idle');
  const [speakerCount, setSpeakerCount] = useState('2');
  const [autoDetectSpeakers, setAutoDetectSpeakers] = useState(false);
  const [participantNames, setParticipantNames] = useState('');
  const namesEdited = useRef(false);
  const [progress, setProgress] = useState(0);
  const [message, setMessage] = useState('Preparing enhanced transcript...');
  const [error, setError] = useState<string | null>(null);
  const [failedStage, setFailedStage] = useState<FailedStage | null>(null);
  const initializedMeetingRef = useRef<string | null>(null);
  const activeStageRef = useRef<FailedStage>('enhancing');
  const cancellation = useRef(new AbortController());
  const running = useRef(false);
  // Outcome of the steps run so far. Refs, so a Retry after Cancel keeps them.
  const keptLiveRef = useRef(resume?.usedLiveTranscript === true);
  const notesRef = useRef<string[]>(resume?.note ? [resume.note] : []);
  const [cancelling, setCancelling] = useState(false);

  const checkCancelled = () => {
    if (cancellation.current.signal.aborted) throw new Error(CANCELLED);
  };

  const storageKey = `post-call-processing:${meetingId}`;

  const completeWorkflow = () => {
    const notes = notesRef.current;
    sessionStorage.setItem(storageKey, 'completed');
    // `summary` tells the job owner that transcript and speakers are final.
    void savePostCallJob({
      meetingId,
      stage: 'summary',
      usedLiveTranscript: keptLiveRef.current,
      // An empty note clears one left by an earlier, failed run.
      note: notes.join(' '),
    });
    setStage('idle');
    if (!notes.length) {
      toast.success('Post-call processing complete', {
        description: 'Transcript enhanced and speaker labels refreshed.',
      });
    }
    onComplete();
  };

  /**
   * The database is the source of truth; this only brings an open page up to
   * date. A failed page refresh therefore never blocks the chain.
   */
  const refreshTranscript = async () => {
    setStage('refreshing');
    setMessage('Refreshing the enhanced transcript...');
    try {
      await onRefetchTranscripts?.();
    } catch (cause) {
      console.warn('Could not refresh the open transcript view:', cause);
    }
    // The rerun links named speakers (and matched voices) to contacts.
    announceChange('people');
  };

  const enhance = async () => {
    if (!meetingFolderPath) {
      throw new PermanentStepError('The recording folder is unavailable for enhancement.');
    }
    setStage('enhancing');
    setProgress(0);
    setMessage('Preparing enhanced transcript...');
    const model = await resolveEnhancementModel({
      liveProvider: transcriptModelConfig?.provider,
      liveModel: transcriptModelConfig?.model,
      signal: cancellation.current.signal,
      onWaiting: setMessage,
    });
    await runRetranscription({
      meetingId,
      meetingFolderPath,
      language: model.provider === 'parakeet' || selectedLanguage === 'auto'
        ? null
        : selectedLanguage || null,
      model,
      onProgress: (nextProgress) => {
        setProgress(nextProgress.progress_percentage);
        setMessage(nextProgress.message);
      },
      signal: cancellation.current.signal,
    });
  };

  const identifySpeakers = async (count: number | null) => {
    checkCancelled();
    setStage('diarizing');
    setProgress(100);
    setMessage(count === null
      ? 'Auto-detecting speakers...'
      : `Identifying ${count} speaker${count === 1 ? '' : 's'}...`);
    await invoke('diarize_meeting', { meetingId, numSpeakers: count });
    checkCancelled();
  };

  /**
   * One named guest and one unnamed remote voice is unambiguous, so that voice
   * gets the name. Every other case is left to the user: a model's speaker
   * number is not an identity.
   */
  const nameSingleGuest = async () => {
    try {
      const [names, labels] = await Promise.all([
        getMeetingParticipants(meetingId),
        invoke<string[]>('meeting_speaker_labels', { meetingId }).catch(() => []),
      ]);
      const rename = singleGuestRename(names, labels ?? []);
      if (!rename) return;
      await invoke('rename_meeting_speaker', { meetingId, from: rename.from, to: rename.to });
      await refreshTranscript();
    } catch (cause) {
      console.warn('Could not name the guest speaker:', cause);
    }
  };

  /**
   * Runs a step, retrying once. Returns the reason when it failed for good.
   * A user cancellation is not a failure of the step and is rethrown.
   */
  const attempt = async (step: () => Promise<void>): Promise<string | null> => {
    for (let tryNumber = 0; tryNumber < 2; tryNumber += 1) {
      try {
        await step();
        return null;
      } catch (cause) {
        if (cancellation.current.signal.aborted) throw cause;
        const reason = describe(cause);
        if (tryNumber === 1 || cause instanceof PermanentStepError || isPermanentNativeFailure(reason)) {
          return reason;
        }
        setMessage('That step failed. Trying once more...');
        await sleep(postCallTimings.retryDelayMs, cancellation.current.signal);
      }
    }
    return null;
  };

  const runChain = async (count: number | null, from: FailedStage) => {
    setError(null);
    setFailedStage(null);

    if (from === 'enhancing') {
      activeStageRef.current = 'enhancing';
      notesRef.current = [];
      const failure = await attempt(enhance);
      keptLiveRef.current = failure !== null;
      if (failure) {
        notesRef.current.push(
          `The accurate re-transcription did not finish, so the live transcript was kept. ${failure}`,
        );
        toast.warning('Kept the live transcript', { description: failure, duration: 10000 });
      }
      // Retranscription transactionally replaces the rows. Refresh immediately so
      // a later diarization error can never leave the old live transcript onscreen.
      await refreshTranscript();
    }

    checkCancelled();
    activeStageRef.current = 'diarizing';
    await savePostCallJob({
      meetingId,
      stage: 'diarizing',
      usedLiveTranscript: keptLiveRef.current,
      note: notesRef.current.join(' '),
    });
    const labelFailure = await attempt(() => identifySpeakers(count));
    if (labelFailure) {
      notesRef.current.push(`Speakers were not labelled. ${labelFailure}`);
      toast.warning('Speakers were not labelled', { description: labelFailure, duration: 10000 });
    }
    await refreshTranscript();
    if (!labelFailure) await nameSingleGuest();
    completeWorkflow();
  };

  const getSelectedSpeakerCount = (): number | null | undefined => {
    if (isNemotron || autoDetectSpeakers) return null;
    const count = Number(speakerCount);
    if (!Number.isInteger(count) || count < 1 || count > 20) {
      setError('Enter the total number of speakers, from 1 to 20.');
      return undefined;
    }
    return count;
  };

  /** `prepare` runs under the same single-flight guard as the chain itself. */
  const run = async (count: number | null, from: FailedStage, prepare?: () => Promise<void>) => {
    if (running.current) return;
    running.current = true;
    cancellation.current = new AbortController();
    setCancelling(false);
    try {
      await prepare?.();
      await runChain(count, from);
    } catch (cause) {
      // Reached only through Cancel (or an error while cancelling): the user
      // stopped the unattended run, so the next step is theirs to choose.
      const nextError = describe(cause);
      setFailedStage(activeStageRef.current);
      setError(nextError);
      setStage('error');
      if (!cancellation.current.signal.aborted) toast.error('Post-call processing failed', { description: nextError });
    } finally {
      running.current = false;
      setCancelling(false);
    }
  };

  const start = async () => {
    if (running.current) return;
    const count = getSelectedSpeakerCount();
    if (count === undefined) return;
    const from: FailedStage = failedStage === 'diarizing' ? 'diarizing' : 'enhancing';
    await run(count, from, async () => {
      const names = parseParticipantNames(participantNames);
      if (namesEdited.current || names.length) {
        // Best effort: the names improve labels and the Drive folder, but the
        // transcript does not depend on them.
        await setMeetingParticipants(meetingId, names).catch((cause) => {
          console.warn('Could not store participant names:', cause);
        });
      }
      await savePostCallJob({
        meetingId,
        stage: from,
        folderPath: meetingFolderPath ?? null,
        speakerChoiceMade: true,
        speakerCount: count,
      });
    });
  };

  useEffect(() => {
    if (!enabled || !meetingId || initializedMeetingRef.current === meetingId) return;
    initializedMeetingRef.current = meetingId;
    const terminalState = sessionStorage.getItem(storageKey);
    if (terminalState === 'completed') {
      onComplete();
      return;
    }
    if (resumeFrom) {
      void run(resume?.speakerCount ?? null, resumeFrom);
      return;
    }
    setStage('prompt');
    void savePostCallJob({ meetingId, stage: 'prompt', folderPath: meetingFolderPath ?? null });
    // Names known before the question: an earlier answer, or the people invited
    // to the calendar event of this recording. The user can change them.
    void getSuggestedParticipants(meetingId).then((names) => {
      if (names.length && !namesEdited.current) setParticipantNames(names.join(', '));
    });
    // The resume record is read once, when the job is created.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, meetingId, onComplete, storageKey]);

  const continueWithLiveTranscript = async () => {
    await refreshTranscript();
    sessionStorage.setItem(storageKey, 'completed');
    keptLiveRef.current = true;
    void savePostCallJob({
      meetingId,
      stage: 'summary',
      folderPath: meetingFolderPath ?? null,
      usedLiveTranscript: true,
      note: 'The live transcript was kept at your request.',
    });
    setStage('idle');
    onComplete();
    toast.info('Using the live transcript', {
      description: 'Keeping the currently saved transcript without further processing.',
    });
  };

  const isWorking = stage === 'enhancing' || stage === 'diarizing' || stage === 'refreshing';
  const visibleProgress = Math.max(4, Math.min(100, progress));

  if (stage === 'idle') return null;

  const choiceClass = (selected: boolean) =>
    cn(
      'h-9 rounded-lg border text-sm font-semibold transition-[background-color,border-color,color,transform] active:scale-[0.97]',
      selected
        ? 'border-af-accent bg-af-accent text-af-on-accent'
        : 'border-af-border bg-af-panel-2 text-af-text-2 hover:border-af-border-strong hover:bg-af-hover hover:text-af-text',
    );

  return (
    <PostCallHandoffCard
      centered
      busy={isWorking}
      onDismiss={!isWorking ? () => { void continueWithLiveTranscript(); } : undefined}
      title={
        isWorking
          ? 'Improving the transcript'
          : stage === 'error'
            ? 'Processing was stopped'
            : isNemotron
              ? 'Identify speakers with Nemotron'
              : 'How many people spoke?'
      }
      detail={
        isWorking
          ? message
          : isNemotron
            ? 'Automatically identify up to 8 speakers from the full recording.'
            : 'Include yourself. A real count labels speakers more accurately.'
      }
    >
      {isWorking ? (
        <div className="space-y-2">
        <Link href={`/meeting-details?id=${encodeURIComponent(meetingId)}`} className="text-xs text-af-accent underline">Open this meeting</Link>
        <div className="h-1.5 overflow-hidden rounded-full bg-af-border">
          <div
            className="h-full rounded-full bg-af-accent transition-[width] duration-300"
            style={{ width: `${visibleProgress}%` }}
          />
        </div>
        <Button variant="ghost" disabled={cancelling} onClick={() => {
          setCancelling(true);
          cancellation.current.abort();
          setMessage('Cancelling after the current native operation finishes…');
        }}>{cancelling ? 'Cancelling…' : 'Cancel processing'}</Button>
        </div>
      ) : (
        <div className="space-y-3">
          {engineError && <p role="alert" className="text-sm text-af-danger">{engineError}</p>}
          {!engine && !engineError && (
            <p role="status" className="text-sm text-af-text-3">Loading diarization settings…</p>
          )}
          {engine && !isNemotron && (
            <>
              <div className="grid grid-cols-4 gap-2">
                {[1, 2, 3, 4, 5, 6, 7, 8].map((count) => (
                  <button
                    key={count}
                    type="button"
                    className={choiceClass(!autoDetectSpeakers && speakerCount === String(count))}
                    onClick={() => {
                      setSpeakerCount(String(count));
                      setAutoDetectSpeakers(false);
                      setError(null);
                    }}
                  >
                    {count}
                  </button>
                ))}
                <button
                  type="button"
                  className={`col-span-4 ${choiceClass(autoDetectSpeakers)}`}
                  onClick={() => {
                    setAutoDetectSpeakers(true);
                    setError(null);
                  }}
                >
                  Auto-detect
                </button>
              </div>
              <input
                type="number"
                min={1}
                max={20}
                value={autoDetectSpeakers ? '' : speakerCount}
                placeholder={autoDetectSpeakers ? 'Speakers will be detected automatically' : undefined}
                onFocus={() => setAutoDetectSpeakers(false)}
                onChange={(event) => {
                  setSpeakerCount(event.target.value);
                  setAutoDetectSpeakers(false);
                }}
                className={cn(fieldClass, 'h-9')}
                aria-label="Total number of speakers"
              />
            </>
          )}
          {resume && stage === 'prompt' && (
            <p className="text-xs text-af-text-3">
              This recording was not finished earlier:{' '}
              <Link href={`/meeting-details?id=${encodeURIComponent(meetingId)}`} className="text-af-accent underline">
                {resume.meetingTitle?.trim() || 'open the meeting'}
              </Link>
            </p>
          )}
          {stage === 'prompt' && (
            <label className="block space-y-1">
              <span className="text-xs font-medium text-af-text-2">Who was on the call? (optional)</span>
              <input
                type="text"
                value={participantNames}
                placeholder="Names other than yours, separated by commas"
                onChange={(event) => {
                  namesEdited.current = true;
                  setParticipantNames(event.target.value);
                }}
                className={cn(fieldClass, 'h-9')}
                aria-label="Names of the other people on the call"
              />
              <span className="block text-xs text-af-text-3">
                Used to name speakers and to file the meeting.
              </span>
            </label>
          )}
          {isNemotron && (
            <Button className="w-full" onClick={() => { void start(); }}>
              {stage === 'error' ? 'Retry auto-detect' : 'Auto-detect & continue'}
            </Button>
          )}
          {error && <p className="text-sm text-af-danger">{error}</p>}
          <div className={cn('flex items-center gap-2', isNemotron ? 'justify-center' : 'justify-end')}>
            <Button variant="ghost" onClick={() => { void continueWithLiveTranscript(); }}>
              Keep live transcript
            </Button>
            {!isNemotron && <Button disabled={!engine} onClick={() => { void start(); }}>
              {stage === 'error' ? 'Retry' : 'Continue'}
            </Button>}
          </div>
        </div>
      )}
    </PostCallHandoffCard>
  );
}
