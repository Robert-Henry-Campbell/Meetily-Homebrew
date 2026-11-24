'use client';

import { useEffect, useRef } from 'react';
import { useRouter } from 'next/navigation';
import { toast } from 'sonner';
import { useGoogleStatus } from '@/hooks/useGoogleStatus';
import { CONNECTIONS_ROUTE, googleAttention } from '@/lib/connections';

/**
 * Tells the user when a connection stops working, wherever they are in the
 * app, with a button that opens the place to fix it. Shown once per problem:
 * at launch if it already exists, and when it appears later.
 */
export function ConnectionsWatcher() {
  const router = useRouter();
  const { status } = useGoogleStatus();
  const announced = useRef<string | null>(null);

  useEffect(() => {
    if (!status) return;
    const attention = googleAttention(status);
    if (!attention) {
      announced.current = null;
      return;
    }
    if (announced.current === attention) return;
    announced.current = attention;
    toast.warning('Google needs your attention', {
      description: attention,
      duration: 20000,
      action: { label: 'Open Connections', onClick: () => router.push(CONNECTIONS_ROUTE) },
    });
  }, [router, status]);

  return null;
}
