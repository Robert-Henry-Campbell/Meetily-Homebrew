'use client';

import Link from 'next/link';
import { AlertTriangle } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { useGoogleStatus } from '@/hooks/useGoogleStatus';
import { CONNECTIONS_ROUTE, googleAttention } from '@/lib/connections';

/** Home page line for a connection that needs the user. Absent when all is well. */
export function ConnectionsBanner({ className }: { className?: string }) {
  const { status } = useGoogleStatus();
  const attention = status ? googleAttention(status) : null;
  if (!attention) return null;
  return (
    <Alert variant="warning" className={className}>
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>Google needs your attention</AlertTitle>
      <AlertDescription className="space-y-2 text-af-text-2">
        <p>{attention} Meetings wait and are sent once this is fixed.</p>
        <Button asChild size="sm" variant="secondary">
          <Link href={CONNECTIONS_ROUTE}>Open Connections</Link>
        </Button>
      </AlertDescription>
    </Alert>
  );
}
