/**
 * The Google Drive export, as the webview sees it. The export itself runs
 * natively in the background; these calls read its state for one meeting,
 * ask for a meeting to be sent now, and read or change the options.
 */
import { invoke } from '@tauri-apps/api/core';

export type DriveExportState = 'not-connected' | 'none' | 'waiting' | 'sent' | 'attention' | 'failed';

export interface DriveExportStatus {
  state: DriveExportState;
  /** One sentence: what is waited for, or what went wrong. */
  message: string | null;
  summaryLink: string | null;
  transcriptLink: string | null;
  folderLink: string | null;
  /** Unix seconds of the last successful export. */
  exportedAt: number | null;
}

export interface DriveExportSettings {
  autoExport: boolean;
  folderPerPerson: boolean;
  rootFolderName: string;
  enabledSince: number | null;
}

/** Emitted natively with `{ meetingId }` when a meeting's export state changed. */
export const DRIVE_EXPORT_CHANGED = 'drive-export-changed';

export const getDriveExportStatus = (meetingId: string) =>
  invoke<DriveExportStatus>('drive_export_status', { meetingId });
export const sendToDriveNow = (meetingId: string) =>
  invoke<DriveExportStatus>('drive_export_send_now', { meetingId });
export const getDriveExportSettings = () => invoke<DriveExportSettings>('drive_export_settings');
export const saveDriveExportSettings = (settings: Pick<DriveExportSettings, 'autoExport' | 'folderPerPerson' | 'rootFolderName'>) =>
  invoke<DriveExportSettings>('drive_export_save_settings', settings);

/** The export writes files without a webview, so it keeps its own copy of the name. */
export async function syncUserDisplayName(name: string): Promise<void> {
  try {
    await invoke('set_user_display_name', { name });
  } catch (error) {
    console.warn('Could not store the display name for exports:', error);
  }
}
