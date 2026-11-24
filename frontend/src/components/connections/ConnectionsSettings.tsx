'use client';

import { ClaudeCard } from './ClaudeCard';
import { GoogleCard } from './GoogleCard';

/** Settings section: the outside services this app signs in to, and their state. */
export function ConnectionsSettings({ openGoogleGuide = false }: { openGoogleGuide?: boolean }) {
  return (
    <>
      <GoogleCard openGuideInitially={openGoogleGuide} />
      <ClaudeCard />
    </>
  );
}
