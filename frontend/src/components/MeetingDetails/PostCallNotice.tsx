'use client';

import { useCallback, useEffect, useState } from 'react';
import { TriangleAlert } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { usePostCallJobs } from '@/contexts/PostCallJobsContext';
import { getPostCallJob, POST_CALL_JOB_CHANGED_EVENT, type PostCallJobRecord } from '@/lib/post-call-jobs';

/**
 * The chain after Stop runs unattended and continues past a failed step. This
 * line is where the meeting says so: what was skipped, why, and a way to run
 * the chain again. It is absent when every step finished.
 */
export function PostCallNotice({
  meetingId,
  meetingFolderPath,
}: {
  meetingId: string;
  meetingFolderPath?: string | null;
}) {
  const jobs = usePostCallJobs();
  const [job, setJob] = useState<PostCallJobRecord | null>(null);
  const [starting, setStarting] = useState(false);

  const load = useCallback(async () => {
    setJob(await getPostCallJob(meetingId));
  }, [meetingId]);

  useEffect(() => {
    setJob(null);
    void load();
    if (typeof window === 'undefined' || typeof window.addEventListener !== 'function') return;
    const onChanged = (event: Event) => {
      const detail = (event as CustomEvent<{ meetingId?: string }>).detail;
      if (detail?.meetingId === meetingId) void load();
    };
    window.addEventListener(POST_CALL_JOB_CHANGED_EVENT, onChanged);
    return () => window.removeEventListener(POST_CALL_JOB_CHANGED_EVENT, onChanged);
  }, [load, meetingId]);

  const finished = job?.stage === 'summary' || job?.stage === 'done';
  if (!job?.note || !finished) return null;

  return (
    <div
      role="status"
      className="flex items-start gap-3 border-b border-af-border bg-af-panel-2 px-4 py-2 text-xs text-af-text-2"
    >
      <TriangleAlert size={14} strokeWidth={1.75} className="mt-0.5 shrink-0 text-af-warning" />
      <p className="min-w-0 flex-1">{job.note}</p>
      {jobs && (
        <Button
          variant="ghost"
          size="sm"
          disabled={starting}
          onClick={() => {
            setStarting(true);
            void jobs.rerun(meetingId, meetingFolderPath).finally(() => setStarting(false));
          }}
        >
          Run again
        </Button>
      )}
    </div>
  );
}
