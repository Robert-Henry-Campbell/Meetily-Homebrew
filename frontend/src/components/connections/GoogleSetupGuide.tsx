'use client';

/**
 * The four steps that connect the app to the user's Google account. Google has
 * no shared sign-in for a personal build, so the user registers their own
 * "Desktop app" client once; each step names what to click and opens the page.
 * Steps 1 to 3 happen in Google Cloud. Step 4 is the sign-in itself.
 */
import { useRef, useState } from 'react';
import { Check, ExternalLink, FileUp, Loader2 } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import {
  cancelGoogleSignIn,
  describeError,
  GOOGLE_CLOUD_LINKS,
  openExternal,
  saveGoogleClient,
  signInToGoogle,
  testGoogleConnection,
  type GoogleCheck,
  type GoogleStatus,
} from '@/lib/connections';
import { cn } from '@/lib/utils';

function OpenButton({ url, children }: { url: string; children: React.ReactNode }) {
  return (
    <Button
      variant="secondary"
      size="sm"
      onClick={() => {
        void openExternal(url).catch((cause) => toast.error('The page could not be opened', { description: describeError(cause) }));
      }}
    >
      <ExternalLink />
      {children}
    </Button>
  );
}

function Step({ number, title, done, children }: { number: number; title: string; done?: boolean; children: React.ReactNode }) {
  return (
    <li className="flex gap-3">
      <span
        className={cn(
          'mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold',
          done ? 'bg-af-success/[0.15] text-af-success' : 'bg-af-accent/[0.12] text-af-accent',
        )}
        aria-hidden
      >
        {done ? <Check className="h-3.5 w-3.5" /> : number}
      </span>
      <div className="min-w-0 flex-1 space-y-2">
        <h4 className="text-sm font-semibold text-af-text">
          <span className="sr-only">Step {number}: </span>
          {title}
        </h4>
        <div className="space-y-2 text-[13px] leading-relaxed text-af-text-2">{children}</div>
      </div>
    </li>
  );
}

export function GoogleSetupGuide({
  open,
  onOpenChange,
  status,
  onStatus,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  status: GoogleStatus;
  onStatus: (status: GoogleStatus) => void;
}) {
  const fileInput = useRef<HTMLInputElement>(null);
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [pasting, setPasting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [signingIn, setSigningIn] = useState(false);
  const [signInError, setSignInError] = useState<string | null>(null);
  const [check, setCheck] = useState<GoogleCheck | null>(null);

  const saveClient = async (input: { json?: string; clientId?: string; clientSecret?: string }) => {
    setSaving(true);
    setImportError(null);
    try {
      onStatus(await saveGoogleClient(input));
      setClientId('');
      setClientSecret('');
      setPasting(false);
      toast.success('Google client imported');
    } catch (cause) {
      setImportError(describeError(cause));
    } finally {
      setSaving(false);
    }
  };

  const importFile = async (file: File | undefined) => {
    if (!file) return;
    // A client file is a few hundred bytes; anything large is the wrong file.
    if (file.size > 64 * 1024) {
      setImportError('This file is too large to be a Google client file.');
      return;
    }
    await saveClient({ json: await file.text() });
  };

  const signIn = async () => {
    setSigningIn(true);
    setSignInError(null);
    setCheck(null);
    try {
      const next = await signInToGoogle();
      onStatus(next);
      // The sign-in is only useful if both services answer.
      setCheck(await testGoogleConnection());
    } catch (cause) {
      setSignInError(describeError(cause));
    } finally {
      setSigningIn(false);
    }
  };

  const close = (next: boolean) => {
    if (!next && signingIn) void cancelGoogleSignIn();
    onOpenChange(next);
  };

  const finished = status.connected && !status.needsAttention;

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[88vh] max-w-xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Connect Google</DialogTitle>
          <DialogDescription>
            A one-time setup of about ten minutes. Steps 1 to 3 register this app in your own Google account; step 4 signs you in.
          </DialogDescription>
        </DialogHeader>

        <ol className="space-y-5">
          <Step number={1} title="Create a project and switch on two services">
            <p>
              In Google Cloud, create a project (any name, for example &quot;Meetily&quot;). Then enable the Google Drive API and the
              Google Calendar API for that project.
            </p>
            <div className="flex flex-wrap gap-2">
              <OpenButton url={GOOGLE_CLOUD_LINKS.newProject}>Create a project</OpenButton>
              <OpenButton url={GOOGLE_CLOUD_LINKS.enableApis}>Enable Drive and Calendar</OpenButton>
            </div>
          </Step>

          <Step number={2} title="Set up the consent screen and add yourself as a test user">
            <p>
              Open Branding and choose <strong>Get started</strong>. Enter an app name and your email address, choose the audience{' '}
              <strong>External</strong>, enter your email address again, accept the policy and choose <strong>Create</strong>. Leave
              the app domain links and the authorized domains empty.
            </p>
            <p>
              Then open Audience. Under <strong>Test users</strong> choose <strong>Add users</strong> and enter your own Google
              address; while the status is &quot;Testing&quot;, only listed test users can sign in.
            </p>
            <p>
              In &quot;Testing&quot;, Google ends the sign-in every 7 days. Meetily then asks you to sign in again, and meetings
              recorded in between are sent afterwards. <strong>Publish app</strong> removes that limit, but Google offers it only
              once Branding has a public home page and a privacy policy on a domain you own.
            </p>
            <div className="flex flex-wrap gap-2">
              <OpenButton url={GOOGLE_CLOUD_LINKS.branding}>Open Branding</OpenButton>
              <OpenButton url={GOOGLE_CLOUD_LINKS.audience}>Open Audience</OpenButton>
            </div>
          </Step>

          <Step number={3} title="Create a Desktop client and import it here" done={status.clientConfigured}>
            <p>
              Open Clients and choose <strong>Create client</strong>. Select the application type <strong>Desktop app</strong>, give it
              a name, and choose <strong>Create</strong>. In the window that follows, choose <strong>Download JSON</strong>, then import
              that file here.
            </p>
            <div className="flex flex-wrap items-center gap-2">
              <OpenButton url={GOOGLE_CLOUD_LINKS.clients}>Open Clients</OpenButton>
              <input
                ref={fileInput}
                type="file"
                accept="application/json,.json"
                className="hidden"
                aria-label="Google client JSON file"
                onChange={(event) => {
                  void importFile(event.target.files?.[0]);
                  // Choosing the same file again must trigger a new import.
                  event.target.value = '';
                }}
              />
              <Button size="sm" disabled={saving} onClick={() => fileInput.current?.click()}>
                {saving ? <Loader2 className="animate-spin" /> : <FileUp />}
                Import the JSON file
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setPasting((value) => !value)}>
                {pasting ? 'Hide the fields' : 'Paste the values instead'}
              </Button>
            </div>
            {pasting && (
              <div className="space-y-2 rounded-xl border border-af-border p-3">
                <Input
                  value={clientId}
                  onChange={(event) => setClientId(event.target.value)}
                  placeholder="Client ID (ends with .apps.googleusercontent.com)"
                  aria-label="Google client ID"
                  autoComplete="off"
                  spellCheck={false}
                />
                <Input
                  type="password"
                  value={clientSecret}
                  onChange={(event) => setClientSecret(event.target.value)}
                  placeholder="Client secret"
                  aria-label="Google client secret"
                  autoComplete="off"
                />
                <Button size="sm" disabled={saving || !clientId.trim()} onClick={() => void saveClient({ clientId, clientSecret })}>
                  Save the client
                </Button>
              </div>
            )}
            {importError && <p role="alert" className="text-af-danger">{importError}</p>}
            {status.clientConfigured && (
              <p className="text-af-text-3">Imported: {status.clientIdHint}</p>
            )}
          </Step>

          <Step number={4} title="Sign in with Google" done={finished}>
            <p>
              Your browser opens Google&apos;s sign-in page. Google shows the notice &quot;Google hasn&apos;t verified this app&quot;, because the
              registration is your own: choose <strong>Advanced</strong>, then <strong>Go to</strong> your app name. Tick both
              permissions and choose <strong>Continue</strong>.
            </p>
            <ul className="list-disc space-y-1 pl-5 text-af-text-3">
              <li>Drive: the app can see and change only the files it creates itself.</li>
              <li>Calendar: read only. The app never creates, changes or answers events.</li>
            </ul>
            <div className="flex flex-wrap items-center gap-2">
              <Button size="sm" disabled={!status.clientConfigured || signingIn} onClick={() => void signIn()}>
                {signingIn && <Loader2 className="animate-spin" />}
                {signingIn ? 'Waiting for the browser…' : finished ? 'Sign in again' : 'Sign in with Google'}
              </Button>
              {signingIn && (
                <Button variant="ghost" size="sm" onClick={() => void cancelGoogleSignIn()}>
                  Cancel
                </Button>
              )}
            </div>
            {!status.clientConfigured && <p className="text-af-text-3">Import the client in step 3 first.</p>}
            {signInError && <p role="alert" className="text-af-danger">{signInError}</p>}
            {finished && (
              <p className="text-af-success">
                Connected{status.accountEmail ? ` as ${status.accountEmail}` : ''}.
              </p>
            )}
            {check && (
              <ul className="space-y-1" aria-label="Test results">
                {[check.drive, check.calendar].map((result) => (
                  <li key={result.message} className={result.ok ? 'text-af-success' : 'text-af-danger'}>
                    {result.message}
                  </li>
                ))}
              </ul>
            )}
          </Step>
        </ol>

        <div className="flex justify-end">
          <Button variant={finished ? 'default' : 'secondary'} onClick={() => close(false)}>
            {finished ? 'Done' : 'Close'}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
