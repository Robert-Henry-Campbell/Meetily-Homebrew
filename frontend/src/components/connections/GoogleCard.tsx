'use client';

import { useState } from 'react';
import { CalendarDays, Cloud, HardDriveUpload, Loader2 } from 'lucide-react';
import { toast } from 'sonner';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { useGoogleStatus } from '@/hooks/useGoogleStatus';
import {
  describeError,
  disconnectGoogle,
  EMPTY_GOOGLE_STATUS,
  googleAttention,
  removeGoogleClient,
  testGoogleConnection,
  type GoogleCheck,
  type GoogleStatus,
} from '@/lib/connections';
import { DriveExportOptions } from './DriveExportOptions';
import { GoogleSetupGuide } from './GoogleSetupGuide';

function stateBadge(status: GoogleStatus) {
  if (googleAttention(status)) return <Badge variant="warning">Needs attention</Badge>;
  if (status.connected) return <Badge variant="success">Connected</Badge>;
  if (status.clientConfigured) return <Badge variant="accent">Not signed in</Badge>;
  return <Badge>Not connected</Badge>;
}

function Permission({ icon, name, purpose, granted }: { icon: React.ReactNode; name: string; purpose: string; granted: boolean }) {
  return (
    <li className="flex items-center gap-3 rounded-xl border border-af-border px-3 py-2">
      <span className="text-af-text-3">{icon}</span>
      <div className="min-w-0 flex-1">
        <p className="text-[13px] font-medium text-af-text">{name}</p>
        <p className="text-xs text-af-text-3">{purpose}</p>
      </div>
      <Badge variant={granted ? 'success' : 'warning'} size="xs">{granted ? 'Allowed' : 'Not allowed'}</Badge>
    </li>
  );
}

/** Google Drive and Calendar: state, the guided setup, a test, and disconnect. */
export function GoogleCard({ openGuideInitially = false }: { openGuideInitially?: boolean }) {
  const { status: loaded, setStatus } = useGoogleStatus();
  const [guideOpen, setGuideOpen] = useState(openGuideInitially);
  const [confirm, setConfirm] = useState<'disconnect' | 'remove' | null>(null);
  const [testing, setTesting] = useState(false);
  const [check, setCheck] = useState<GoogleCheck | null>(null);
  const status = loaded ?? EMPTY_GOOGLE_STATUS;
  const attention = googleAttention(status);

  const test = async () => {
    setTesting(true);
    setCheck(null);
    try {
      setCheck(await testGoogleConnection());
    } catch (cause) {
      toast.error('The test could not run', { description: describeError(cause) });
    } finally {
      setTesting(false);
    }
  };

  return (
    <section aria-labelledby="google-connection" className="rounded-2xl border border-af-border bg-af-panel-2/40 p-5">
      <div className="flex flex-wrap items-start gap-3">
        <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-af-accent/[0.12] text-af-accent">
          <Cloud className="h-5 w-5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 id="google-connection" className="text-[15px] font-semibold text-af-text">Google</h3>
            {loaded && stateBadge(status)}
          </div>
          <p className="mt-1 text-sm text-af-text-2">
            Sends each meeting&apos;s summary and transcript to your Google Drive, and reads your calendar to name meetings and the
            people in them.
          </p>
          {status.connected && status.accountEmail && (
            <p className="mt-1 text-xs text-af-text-3">Signed in as {status.accountEmail}</p>
          )}
        </div>
      </div>

      {attention && (
        <div role="alert" className="mt-4 flex flex-wrap items-center gap-3 rounded-xl border border-af-warning/35 bg-af-warning/10 px-3.5 py-3">
          <p className="min-w-0 flex-1 text-[13px] text-af-text">{attention}</p>
          <Button size="sm" onClick={() => setGuideOpen(true)}>Sign in again</Button>
        </div>
      )}

      {status.connected && (
        <ul className="mt-4 grid gap-2 sm:grid-cols-2" aria-label="Permissions">
          <Permission
            icon={<HardDriveUpload className="h-4 w-4" />}
            name="Drive"
            purpose="Only files this app creates"
            granted={status.driveGranted}
          />
          <Permission
            icon={<CalendarDays className="h-4 w-4" />}
            name="Calendar"
            purpose="Read only"
            granted={status.calendarGranted}
          />
        </ul>
      )}

      {status.connected && status.driveGranted && <DriveExportOptions />}

      {check && (
        <ul className="mt-3 space-y-1 text-[13px]" aria-label="Test results">
          {[check.drive, check.calendar].map((result) => (
            <li key={result.message} className={result.ok ? 'text-af-success' : 'text-af-danger'}>{result.message}</li>
          ))}
        </ul>
      )}

      <div className="mt-4 flex flex-wrap items-center gap-2">
        {!status.connected && (
          <Button onClick={() => setGuideOpen(true)}>
            {status.clientConfigured ? 'Continue the setup' : 'Connect Google'}
          </Button>
        )}
        {status.connected && (
          <>
            <Button variant="secondary" disabled={testing} onClick={() => void test()}>
              {testing && <Loader2 className="animate-spin" />}
              Test the connection
            </Button>
            <Button variant="ghost" onClick={() => setGuideOpen(true)}>Open the setup guide</Button>
            <Button variant="ghost" onClick={() => setConfirm('disconnect')}>Disconnect</Button>
          </>
        )}
        {status.clientConfigured && !status.connected && (
          <Button variant="ghost" onClick={() => setConfirm('remove')}>Remove the imported client</Button>
        )}
      </div>

      <GoogleSetupGuide open={guideOpen} onOpenChange={setGuideOpen} status={status} onStatus={setStatus} />
      <ConfirmDialog
        open={confirm === 'disconnect'}
        onOpenChange={(open) => !open && setConfirm(null)}
        title="Disconnect Google?"
        description="Meetings are no longer sent to Drive and the calendar is no longer read. Files already in your Drive stay there. The imported client stays, so signing in again takes one step."
        confirmLabel="Disconnect"
        onConfirm={async () => {
          try {
            setStatus(await disconnectGoogle());
            setCheck(null);
          } catch (cause) {
            toast.error('Google could not be disconnected', { description: describeError(cause) });
          }
        }}
      />
      <ConfirmDialog
        open={confirm === 'remove'}
        onOpenChange={(open) => !open && setConfirm(null)}
        title="Remove the imported client?"
        description="The client ID and secret are deleted from this app. To connect again, import the JSON file again (step 3). Nothing changes in Google Cloud."
        confirmLabel="Remove"
        variant="danger"
        onConfirm={async () => {
          try {
            setStatus(await removeGoogleClient());
          } catch (cause) {
            toast.error('The client could not be removed', { description: describeError(cause) });
          }
        }}
      />
    </section>
  );
}
