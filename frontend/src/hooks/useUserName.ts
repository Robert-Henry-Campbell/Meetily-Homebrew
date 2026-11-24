'use client';

import { useEffect, useState } from 'react';
import { syncUserDisplayName } from '@/lib/drive-export';

const KEY = 'meetily_user_name';
const EVENT = 'af-user-name';

export function readUserName(): string {
  if (typeof window === 'undefined') return '';
  try {
    return localStorage.getItem(KEY)?.trim() ?? '';
  } catch {
    return '';
  }
}

export function saveUserName(name: string) {
  try {
    localStorage.setItem(KEY, name);
  } catch {
    // Storage unavailable; the name simply is not remembered.
  }
  window.dispatchEvent(new CustomEvent(EVENT));
  // Files written in the background (the Drive export) show the same name.
  void syncUserDisplayName(name.trim());
}

/** The user's display name (Settings → General), kept live across screens. */
export function useUserName(): string {
  const [name, setName] = useState('');
  useEffect(() => {
    const read = () => setName(readUserName());
    read();
    const onStorage = (event: StorageEvent) => {
      if (event.key === KEY) read();
    };
    window.addEventListener(EVENT, read);
    window.addEventListener('storage', onStorage);
    return () => {
      window.removeEventListener(EVENT, read);
      window.removeEventListener('storage', onStorage);
    };
  }, []);
  return name;
}
