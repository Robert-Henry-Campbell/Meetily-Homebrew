'use client';

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getGoogleStatus, GOOGLE_CONNECTION_CHANGED, type GoogleStatus } from '@/lib/connections';

/**
 * The Google connection, kept current. The native side emits the new status on
 * every change (sign-in, disconnect, a refresh that Google rejected), so every
 * mounted view shows the same state without polling.
 */
export function useGoogleStatus(): { status: GoogleStatus | null; setStatus: (status: GoogleStatus) => void; reload: () => Promise<void> } {
  const [status, setStatus] = useState<GoogleStatus | null>(null);

  const reload = useCallback(async () => {
    try {
      setStatus(await getGoogleStatus());
    } catch (error) {
      console.warn('Could not read the Google connection:', error);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void reload();
    void listen<GoogleStatus>(GOOGLE_CONNECTION_CHANGED, (event) => {
      if (!disposed && event.payload) setStatus(event.payload);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    }).catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [reload]);

  return { status, setStatus, reload };
}
