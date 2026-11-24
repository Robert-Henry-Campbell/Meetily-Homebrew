'use client';

import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { toast } from 'sonner';
import { PostCallProcessingWorker } from '@/components/MeetingDetails/PostCallProcessingWorker';
import {
  getPostCallJob,
  listUnfinishedPostCallJobs,
  savePostCallJob,
  type PostCallJobRecord,
} from '@/lib/post-call-jobs';
import { startPostCallSummary } from '@/lib/post-call-summary';

export interface PostCallView {
  meetingId: string;
  meetingFolderPath?: string | null;
  enabled: boolean;
  onRefetchTranscripts?: () => Promise<void>;
  onComplete: () => void;
}

interface QueuedJob {
  meetingId: string;
  meetingFolderPath?: string | null;
  /** Stored progress of a job found unfinished at launch, or of a requested re-run. */
  resume?: PostCallJobRecord | null;
}

interface Jobs {
  attach: (view: PostCallView) => () => void;
  /** Runs the chain again for a meeting whose accurate pass did not finish. */
  rerun: (meetingId: string, meetingFolderPath?: string | null) => Promise<void>;
}
const Context = createContext<Jobs | null>(null);
export const usePostCallJobs = () => useContext(Context);

/** Waits of the job owner. Tests shorten them. */
export const postCallOwnerTimings = {
  /** Unfinished jobs resume this long after launch, once engines have initialised. */
  resumeDelayMs: 4000,
  /** How often a summary that waits for its model download is tried again. */
  summaryRetryMs: 20000,
  /** A summary that still cannot start after this many tries is left to the user. */
  summaryMaxTries: 540,
};

const storageKey = (meetingId: string) => `post-call-processing:${meetingId}`;

/** Route changes detach views, not work. Only the mounted view may refresh its
 * state; completion is retained for the next visit. Native retranscription is
 * single-flight, so the host presents one handoff at a time.
 *
 * This owner, not the meeting page, also starts the summary when a chain
 * finishes, and resumes chains that a reload or a quit interrupted: the page
 * may be closed at either moment. */
export function PostCallJobsProvider({ children }: { children: React.ReactNode }) {
  const views = useRef(new Map<string, PostCallView>());
  const completed = useRef(new Set<string>());
  const dirty = useRef(new Set<string>());
  const summaries = useRef(new Set<string>());
  const reruns = useRef(new Set<string>());
  const mounted = useRef(true);
  const [queue, setQueue] = useState<QueuedJob[]>([]);

  const enqueue = useCallback((job: QueuedJob) => {
    setQueue(current => current.some(candidate => candidate.meetingId === job.meetingId)
      ? current : [...current, job]);
  }, []);

  /** Single-flight per meeting. A waiting summary keeps its `summary` stage, so
   * it is also picked up again after a restart. */
  const startSummary = useCallback(async (meetingId: string) => {
    if (summaries.current.has(meetingId)) return;
    summaries.current.add(meetingId);
    try {
      let announcedWait = false;
      for (let tryNumber = 0; tryNumber < postCallOwnerTimings.summaryMaxTries; tryNumber += 1) {
        const outcome = await startPostCallSummary(meetingId, {
          replaceExisting: reruns.current.has(meetingId),
        });
        if (outcome.status === 'waiting') {
          if (!announcedWait) {
            announcedWait = true;
            toast.info('The summary starts when its model is ready', { description: outcome.message });
          }
          await new Promise(resolve => setTimeout(resolve, postCallOwnerTimings.summaryRetryMs));
          if (!mounted.current) return;
          continue;
        }
        if (outcome.status === 'started') {
          toast.info('Generating summary...', {
            description: `Using ${outcome.provider}/${outcome.model}`,
            duration: 3000,
          });
        } else if (outcome.status === 'failed') {
          toast.error('The summary did not start', { description: outcome.message, duration: 10000 });
        }
        // Started, not needed, or failed for a reason a retry will not fix: the
        // chain is over either way. A failed start is left to the meeting page.
        reruns.current.delete(meetingId);
        await savePostCallJob({ meetingId, stage: 'done' });
        return;
      }
    } finally {
      summaries.current.delete(meetingId);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    const timer = setTimeout(() => {
      void listUnfinishedPostCallJobs().then((records) => {
        if (!mounted.current) return;
        for (const record of records) {
          if (completed.current.has(record.meetingId)) continue;
          if (record.stage === 'summary') {
            void startSummary(record.meetingId);
          } else {
            enqueue({ meetingId: record.meetingId, meetingFolderPath: record.folderPath, resume: record });
          }
        }
      });
    }, postCallOwnerTimings.resumeDelayMs);
    return () => {
      mounted.current = false;
      clearTimeout(timer);
    };
  }, [enqueue, startSummary]);

  const attach = useCallback((view: PostCallView) => {
    views.current.set(view.meetingId, view);
    const isCompleted = completed.current.has(view.meetingId)
      || sessionStorage.getItem(storageKey(view.meetingId)) === 'completed';
    if (isCompleted) {
      view.onComplete();
    } else if (view.enabled) {
      enqueue({ meetingId: view.meetingId, meetingFolderPath: view.meetingFolderPath });
    }
    if (dirty.current.delete(view.meetingId)) {
      void view.onRefetchTranscripts?.().catch(() => dirty.current.add(view.meetingId));
    }
    return () => {
      if (views.current.get(view.meetingId) === view) views.current.delete(view.meetingId);
    };
  }, [enqueue]);

  const rerun = useCallback(async (meetingId: string, meetingFolderPath?: string | null) => {
    const record = await getPostCallJob(meetingId);
    sessionStorage.removeItem(storageKey(meetingId));
    completed.current.delete(meetingId);
    // The new transcript replaces the old one, so its summary is replaced too
    // (unless the user edited it; the summary starter checks that).
    reruns.current.add(meetingId);
    const answered = record?.speakerChoiceMade === true;
    if (answered) await savePostCallJob({ meetingId, stage: 'enhancing' });
    enqueue({
      meetingId,
      meetingFolderPath: meetingFolderPath ?? record?.folderPath,
      // Without a stored answer the speaker question is asked again.
      resume: answered && record ? { ...record, stage: 'enhancing', usedLiveTranscript: false, note: null } : null,
    });
  }, [enqueue]);

  const job = queue[0];
  return <Context.Provider value={{ attach, rerun }}>
    {children}
    {job && <PostCallProcessingWorker key={`post-call-job:${job.meetingId}`}
      enabled meetingId={job.meetingId} meetingFolderPath={job.meetingFolderPath}
      resume={job.resume}
      onRefetchTranscripts={async () => {
        const view = views.current.get(job.meetingId);
        if (view?.onRefetchTranscripts) await view.onRefetchTranscripts();
        else dirty.current.add(job.meetingId);
      }}
      onComplete={() => {
        completed.current.add(job.meetingId);
        views.current.get(job.meetingId)?.onComplete();
        setQueue(current => current.filter(candidate => candidate.meetingId !== job.meetingId));
        void startSummary(job.meetingId);
      }} />}
  </Context.Provider>;
}
