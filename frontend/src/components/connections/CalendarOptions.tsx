'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Loader2 } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { describeError } from '@/lib/connections';

interface CalendarChoice {
  id: string;
  name: string;
  primary: boolean;
  selected: boolean;
}

interface CalendarOptionsData {
  matching: boolean;
  calendars: CalendarChoice[];
}

/**
 * Which calendars a recording is matched against. The calendar is only read:
 * the matched event gives the meeting its title and the names of the people
 * invited. Shown in the Google card once Calendar may be used.
 */
export function CalendarOptions() {
  const [options, setOptions] = useState<CalendarOptionsData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setOptions(await invoke<CalendarOptionsData>('calendar_options'));
    } catch (cause) {
      setError(describeError(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (next: CalendarOptionsData) => {
    const previous = options;
    setOptions(next);
    try {
      await invoke('calendar_save_options', {
        matching: next.matching,
        calendarIds: next.calendars.filter((calendar) => calendar.selected).map((calendar) => calendar.id),
      });
    } catch (cause) {
      setOptions(previous);
      toast.error('The calendar options could not be saved', { description: describeError(cause) });
    }
  };

  return (
    <div className="mt-4 space-y-3 rounded-xl border border-af-border p-4" aria-label="Calendar options">
      <div className="flex items-start justify-between gap-4">
        <div>
          <p className="text-[13px] font-medium text-af-text">Match recordings to calendar events</p>
          <p className="text-xs text-af-text-3">
            A recording made during an event takes the event&apos;s title, and the people invited are offered as names for the
            speaker question, the summary and the Drive folder. Events are only read.
          </p>
        </div>
        {options && (
          <Switch
            checked={options.matching}
            onCheckedChange={(checked) => void save({ ...options, matching: checked })}
            aria-label="Match recordings to calendar events"
            className="shrink-0"
          />
        )}
      </div>
      {loading && (
        <p role="status" className="flex items-center gap-2 text-xs text-af-text-3">
          <Loader2 className="h-3.5 w-3.5 animate-spin" /> Reading your calendars…
        </p>
      )}
      {error && (
        <div role="alert" className="flex flex-wrap items-center gap-3 text-xs text-af-danger">
          <span className="min-w-0 flex-1">{error}</span>
          <Button size="xs" variant="secondary" onClick={() => void load()}>Try again</Button>
        </div>
      )}
      {options && options.matching && (
        <fieldset className="space-y-1">
          <legend className="mb-1 text-xs font-medium text-af-text-2">Calendars to look in</legend>
          {options.calendars.map((calendar) => (
            <label key={calendar.id} className="flex items-center gap-2 text-[13px] text-af-text-2">
              <input
                type="checkbox"
                checked={calendar.selected}
                onChange={(event) => void save({
                  ...options,
                  calendars: options.calendars.map((entry) => (entry.id === calendar.id ? { ...entry, selected: event.target.checked } : entry)),
                })}
              />
              <span className="truncate">{calendar.name}</span>
              {calendar.primary && <span className="text-xs text-af-text-4">main calendar</span>}
            </label>
          ))}
          {!options.calendars.some((calendar) => calendar.selected) && (
            <p className="text-xs text-af-text-3">With nothing ticked, the main calendar is used.</p>
          )}
        </fieldset>
      )}
    </div>
  );
}
