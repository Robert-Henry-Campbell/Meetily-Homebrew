'use client';

/**
 * Where this meeting stands with Google Drive: sent, waiting (and for what),
 * or in need of the user. The export runs natively in the background; this
 * chip only shows its state and offers the two things a user can do about it,
 * open the files and send now.
 */
import { useCallback, useEffect, useState } from 'react';
import Link from 'next/link';
import { listen } from '@tauri-apps/api/event';
import { AlertTriangle, Check, Clock, CloudOff, CloudUpload, ExternalLink } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { CONNECTIONS_ROUTE, describeError, GOOGLE_CONNECTION_CHANGED, openExternal } from '@/lib/connections';
import {
  DRIVE_EXPORT_CHANGED,
  getDriveExportStatus,
  sendToDriveNow,
  type DriveExportState,
  type DriveExportStatus,
} from '@/lib/drive-export';
import { POST_CALL_JOB_CHANGED_EVENT } from '@/lib/post-call-jobs';
import { cn } from '@/lib/utils';

const LOOK: Record<DriveExportState, { label: string; icon: typeof Check; tone: string }> = {
  sent: { label: 'In Drive', icon: Check, tone: 'text-af-success' },
  waiting: { label: 'Drive: waiting', icon: Clock, tone: 'text-af-text-3' },
  attention: { label: 'Drive needs attention', icon: AlertTriangle, tone: 'text-af-warning' },
  failed: { label: 'Not sent to Drive', icon: AlertTriangle, tone: 'text-af-danger' },
  none: { label: 'Not in Drive', icon: CloudUpload, tone: 'text-af-text-3' },
  'not-connected': { label: 'Drive not connected', icon: CloudOff, tone: 'text-af-text-4' },
};

/** How often a waiting meeting is looked at again; the reason for waiting changes with time. */
export const DRIVE_CHIP_REFRESH_MS = 15000;

export function DriveChip({ meetingId }: { meetingId: string }) {
  const [status, setStatus] = useState<DriveExportStatus | null>(null);
  const [sending, setSending] = useState(false);

  const load = useCallback(async () => {
    try {
      setStatus(await getDriveExportStatus(meetingId));
    } catch (error) {
      console.warn('Could not read the Drive export state:', error);
    }
  }, [meetingId]);

  useEffect(() => {
    setStatus(null);
    void load();
    let disposed = false;
    const stops: Array<() => void> = [];
    const keep = (stop: () => void) => (disposed ? stop() : stops.push(stop));
    void listen<{ meetingId?: string }>(DRIVE_EXPORT_CHANGED, (event) => {
      if (event.payload?.meetingId === meetingId) void load();
    }).then(keep).catch(() => undefined);
    void listen(GOOGLE_CONNECTION_CHANGED, () => void load()).then(keep).catch(() => undefined);
    const onJob = (event: Event) => {
      if ((event as CustomEvent<{ meetingId?: string }>).detail?.meetingId === meetingId) void load();
    };
    if (typeof window !== 'undefined' && typeof window.addEventListener === 'function') {
      window.addEventListener(POST_CALL_JOB_CHANGED_EVENT, onJob);
      stops.push(() => window.removeEventListener(POST_CALL_JOB_CHANGED_EVENT, onJob));
    }
    return () => {
      disposed = true;
      stops.forEach((stop) => stop());
    };
  }, [load, meetingId]);

  const waiting = status?.state === 'waiting';
  useEffect(() => {
    if (!waiting) return;
    const timer = setInterval(() => void load(), DRIVE_CHIP_REFRESH_MS);
    return () => clearInterval(timer);
  }, [load, waiting]);

  if (!status) return null;
  const look = LOOK[status.state];
  const Icon = look.icon;
  const open = (url: string) => void openExternal(url).catch((cause) => toast.error('The page could not be opened', { description: describeError(cause) }));
  const sendNow = async () => {
    setSending(true);
    try {
      setStatus(await sendToDriveNow(meetingId));
    } catch (cause) {
      toast.error('The meeting could not be queued for Drive', { description: describeError(cause) });
    } finally {
      setSending(false);
    }
  };
  const links: Array<[string, string | null]> = [
    ['Open the folder', status.folderLink],
    ['Open the summary', status.summaryLink],
    ['Open the transcript', status.transcriptLink],
  ];
  const sentOn = status.exportedAt
    ? new Date(status.exportedAt * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
    : null;

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn('inline-flex h-6 items-center gap-1.5 rounded-full px-2 text-xs transition-colors hover:bg-af-hover', look.tone)}
          aria-label={`Google Drive: ${look.label}`}
        >
          <Icon className="h-3.5 w-3.5" />
          {look.label}
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 space-y-3">
        <div>
          <p className="text-sm font-medium text-af-text">{look.label}</p>
          {status.message && <p className="mt-1 text-xs leading-relaxed text-af-text-2">{status.message}</p>}
          {status.state === 'sent' && !status.message && sentOn && (
            <p className="mt-1 text-xs text-af-text-3">Summary and transcript were sent on {sentOn}. Later changes are sent on their own.</p>
          )}
          {status.state === 'none' && (
            <p className="mt-1 text-xs leading-relaxed text-af-text-2">
              This meeting is not sent on its own (it was recorded before the Drive export was switched on, or the export is off).
            </p>
          )}
          {status.state === 'not-connected' && (
            <p className="mt-1 text-xs leading-relaxed text-af-text-2">
              Connect Google to send each meeting&apos;s summary and transcript to your Drive.
            </p>
          )}
        </div>
        {links.some(([, url]) => url) && (
          <ul className="space-y-0.5">
            {links.filter(([, url]) => url).map(([label, url]) => (
              <li key={label}>
                <button
                  type="button"
                  onClick={() => open(url as string)}
                  className="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-[13px] text-af-text-2 hover:bg-af-hover hover:text-af-text"
                >
                  <ExternalLink className="h-3.5 w-3.5 shrink-0" />
                  {label}
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="flex flex-wrap gap-2">
          {status.state !== 'not-connected' && (
            <Button size="sm" variant={status.state === 'sent' ? 'secondary' : 'default'} disabled={sending} onClick={() => void sendNow()}>
              {status.state === 'sent' ? 'Send again' : status.state === 'none' ? 'Send to Drive' : 'Send now'}
            </Button>
          )}
          {(status.state === 'not-connected' || status.state === 'attention') && (
            <Button asChild size="sm" variant={status.state === 'not-connected' ? 'default' : 'secondary'}>
              <Link href={CONNECTIONS_ROUTE}>Open Connections</Link>
            </Button>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
