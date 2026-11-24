'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ExternalLink, Loader2, Sparkles } from 'lucide-react';
import { toast } from 'sonner';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ConfirmDialog } from '@/components/ui/confirm-dialog';
import { fieldClass, Input } from '@/components/ui/input';
import {
  checkClaudeKey,
  connectClaude,
  DEFAULT_CLAUDE_MODEL,
  describeError,
  getClaudeStatus,
  openExternal,
  removeClaudeKey,
  type CheckResult,
  type ClaudeStatus,
} from '@/lib/connections';
import { cn } from '@/lib/utils';

interface ClaudeModel {
  id: string;
  display_name?: string | null;
}

const CLAUDE_KEYS_URL = 'https://console.anthropic.com/settings/keys';

/**
 * The Claude API key that writes summaries. The key is checked with a real
 * request before it is saved, and the saved key is never shown again: the card
 * only says that one exists and how it ends.
 */
export function ClaudeCard() {
  const [status, setStatus] = useState<ClaudeStatus | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [model, setModel] = useState(DEFAULT_CLAUDE_MODEL);
  const [models, setModels] = useState<ClaudeModel[]>([]);
  const [working, setWorking] = useState(false);
  const [result, setResult] = useState<CheckResult | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);

  const loadModels = useCallback(async () => {
    try {
      setModels((await invoke<ClaudeModel[]>('claude_models')) ?? []);
    } catch {
      setModels([]);
    }
  }, []);

  useEffect(() => {
    void getClaudeStatus()
      .then((next) => {
        setStatus(next);
        setModel(next.model || DEFAULT_CLAUDE_MODEL);
      })
      .catch((cause) => console.warn('Could not read the Claude connection:', cause));
    void loadModels();
  }, [loadModels]);

  // The chosen model is always offered, also when the list could not be fetched.
  const options = models.some((entry) => entry.id === model) ? models : [{ id: model }, ...models];
  const typedKey = apiKey.trim();
  const canSubmit = !working && (typedKey.length > 0 || status?.keySaved === true);

  const checkAndSave = async () => {
    setWorking(true);
    setResult(null);
    try {
      const check = await checkClaudeKey(typedKey || null, model);
      setResult(check);
      if (!check.ok) return;
      const next = await connectClaude(typedKey || null, model);
      setStatus(next);
      setApiKey('');
      void loadModels();
      toast.success('Claude writes the summaries', { description: `Model: ${next.model}` });
    } catch (cause) {
      setResult({ ok: false, message: describeError(cause) });
    } finally {
      setWorking(false);
    }
  };

  return (
    <section aria-labelledby="claude-connection" className="rounded-2xl border border-af-border bg-af-panel-2/40 p-5">
      <div className="flex flex-wrap items-start gap-3">
        <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-af-accent/[0.12] text-af-accent">
          <Sparkles className="h-5 w-5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 id="claude-connection" className="text-[15px] font-semibold text-af-text">Claude</h3>
            {status && (
              status.keySaved && status.selected
                ? <Badge variant="success">In use</Badge>
                : status.keySaved
                  ? <Badge variant="accent">Key saved, not in use</Badge>
                  : <Badge>Not connected</Badge>
            )}
          </div>
          <p className="mt-1 text-sm text-af-text-2">
            Writes the summaries with your Claude API key. Each summary is billed to that key&apos;s account.
          </p>
        </div>
      </div>

      <div className="mt-4 grid gap-3 sm:grid-cols-[1fr_14rem]">
        <label className="block space-y-1">
          <span className="text-xs font-medium text-af-text-2">API key</span>
          <Input
            type="password"
            value={apiKey}
            onChange={(event) => {
              setApiKey(event.target.value);
              setResult(null);
            }}
            placeholder={status?.keySaved ? `A key is saved (${status.keyHint ?? '…'}). Paste a new one to replace it.` : 'sk-ant-…'}
            autoComplete="off"
            spellCheck={false}
            aria-label="Claude API key"
          />
        </label>
        <label className="block space-y-1">
          <span className="text-xs font-medium text-af-text-2">Model</span>
          <select
            value={model}
            onChange={(event) => {
              setModel(event.target.value);
              setResult(null);
            }}
            className={cn(fieldClass, 'appearance-none')}
            aria-label="Claude model"
          >
            {options.map((entry) => (
              <option key={entry.id} value={entry.id}>{entry.display_name || entry.id}</option>
            ))}
          </select>
        </label>
      </div>

      {result && (
        <p role={result.ok ? 'status' : 'alert'} className={cn('mt-3 text-[13px]', result.ok ? 'text-af-success' : 'text-af-danger')}>
          {result.message}
        </p>
      )}

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <Button disabled={!canSubmit} onClick={() => void checkAndSave()}>
          {working && <Loader2 className="animate-spin" />}
          {working ? 'Checking the key…' : status?.keySaved && !typedKey ? 'Check and use' : 'Check and save'}
        </Button>
        <Button
          variant="ghost"
          onClick={() => void openExternal(CLAUDE_KEYS_URL).catch((cause) => toast.error('The page could not be opened', { description: describeError(cause) }))}
        >
          <ExternalLink />
          Get an API key
        </Button>
        {status?.keySaved && (
          <Button variant="ghost" onClick={() => setConfirmRemove(true)}>Remove the key</Button>
        )}
      </div>

      <p className="mt-4 text-xs leading-relaxed text-af-text-3">
        The check sends one very short request, which costs a fraction of a cent. If Claude cannot be used later (no connection, a
        rejected key, a spending limit), the local model writes the summary and the meeting says so. A request that Claude&apos;s
        safety filters decline is passed on to another Claude model by the API.
      </p>

      <ConfirmDialog
        open={confirmRemove}
        onOpenChange={setConfirmRemove}
        title="Remove the Claude key?"
        description="The key is deleted from this app. Summaries are written by the local model from then on."
        confirmLabel="Remove"
        variant="danger"
        onConfirm={async () => {
          try {
            const next = await removeClaudeKey();
            setStatus(next);
            setResult(null);
          } catch (cause) {
            toast.error('The key could not be removed', { description: describeError(cause) });
          }
        }}
      />
    </section>
  );
}
