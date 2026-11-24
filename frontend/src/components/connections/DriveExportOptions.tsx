'use client';

import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { describeError } from '@/lib/connections';
import { getDriveExportSettings, saveDriveExportSettings, type DriveExportSettings } from '@/lib/drive-export';

/** The three choices of the Drive export. Shown in the Google card once signed in. */
export function DriveExportOptions() {
  const [settings, setSettings] = useState<DriveExportSettings | null>(null);
  const [folderName, setFolderName] = useState('');

  useEffect(() => {
    void getDriveExportSettings()
      .then((loaded) => {
        setSettings(loaded);
        setFolderName(loaded.rootFolderName);
      })
      .catch((cause) => console.warn('Could not read the Drive export options:', cause));
  }, []);

  if (!settings) return null;

  const save = async (next: Pick<DriveExportSettings, 'autoExport' | 'folderPerPerson' | 'rootFolderName'>) => {
    try {
      const saved = await saveDriveExportSettings(next);
      setSettings(saved);
      setFolderName(saved.rootFolderName);
    } catch (cause) {
      toast.error('The Drive options could not be saved', { description: describeError(cause) });
    }
  };
  const current = { autoExport: settings.autoExport, folderPerPerson: settings.folderPerPerson, rootFolderName: settings.rootFolderName };

  return (
    <div className="mt-4 space-y-3 rounded-xl border border-af-border p-4" aria-label="Drive export options">
      <div className="flex items-start justify-between gap-4">
        <div>
          <p className="text-[13px] font-medium text-af-text">Send meetings to Drive automatically</p>
          <p className="text-xs text-af-text-3">
            After a recording, the summary and the transcript are sent as two Markdown files once they are final. Meetings recorded
            before this was switched on are sent only when you choose &quot;Send to Drive&quot; on the meeting.
          </p>
        </div>
        <Switch
          checked={settings.autoExport}
          onCheckedChange={(checked) => void save({ ...current, autoExport: checked })}
          aria-label="Send meetings to Drive automatically"
          className="shrink-0"
        />
      </div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <p className="text-[13px] font-medium text-af-text">A folder per person</p>
          <p className="text-xs text-af-text-3">
            A call with one other person goes into a folder with that person&apos;s name. Calls with several people go into the
            meeting&apos;s group folder, or into &quot;Group meetings&quot; with the names in the file title. Off: one folder for everything.
          </p>
        </div>
        <Switch
          checked={settings.folderPerPerson}
          onCheckedChange={(checked) => void save({ ...current, folderPerPerson: checked })}
          aria-label="A folder per person"
          className="shrink-0"
        />
      </div>
      <div>
        <label className="block space-y-1">
          <span className="text-[13px] font-medium text-af-text">Name of the main folder</span>
          <div className="flex gap-2">
            <Input value={folderName} onChange={(event) => setFolderName(event.target.value)} aria-label="Name of the main Drive folder" />
            <Button
              variant="secondary"
              disabled={!folderName.trim() || folderName.trim() === settings.rootFolderName}
              onClick={() => void save({ ...current, rootFolderName: folderName })}
            >
              Save
            </Button>
          </div>
        </label>
        <p className="mt-1 text-xs text-af-text-3">
          Used when the folder is created. After that you can rename or move it in Drive; the app keeps using it.
        </p>
      </div>
    </div>
  );
}
