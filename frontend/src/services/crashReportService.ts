import { invoke } from '@tauri-apps/api/core'
import { save } from '@tauri-apps/plugin-dialog'

export type CrashType = 'panic' | 'unexpected_exit'

/**
 * Where "Send Report" opens a new issue, or null when a build has no place for
 * reports. Null in this personal build: its crashes come from code the base
 * project did not write, so that project's tracker is not the place for them,
 * and this fork has none. The report can still be saved as a ZIP.
 */
export const CRASH_REPORT_ISSUE_URL: string | null = null

export interface PendingCrashReport {
  reportId: string
  detectedAt: string
  crashType: CrashType
  appVersion: string
}

export async function getPendingCrashReport(): Promise<PendingCrashReport | null> {
  return invoke<PendingCrashReport | null>('get_pending_crash_report')
}

export async function chooseCrashReportDestination(
  report: PendingCrashReport,
): Promise<string | null> {
  const destination = await save({
    defaultPath: `Meetily-crash-${report.reportId.slice(0, 8)}.zip`,
    filters: [{ name: 'ZIP archive', extensions: ['zip'] }],
  })
  if (!destination) return null
  return destination.toLowerCase().endsWith('.zip') ? destination : `${destination}.zip`
}

export async function createCrashReportZip(destination: string): Promise<string> {
  return invoke<string>('create_crash_report_zip', { destination })
}

export async function dismissCrashReport(): Promise<void> {
  await invoke('dismiss_pending_crash_report')
}

export async function openCrashReportIssue(report: PendingCrashReport): Promise<void> {
  if (!CRASH_REPORT_ISSUE_URL) return
  const title = 'Meetily crash report'
  const body = [
    '## Crash report',
    '',
    'Please attach the ZIP Meetily just created, then describe what was happening before the crash.',
  ].join('\n')
  const query = new URLSearchParams({
    title,
    body,
  })

  await invoke('open_external_url', {
    url: `${CRASH_REPORT_ISSUE_URL}?${query.toString()}`,
  })
}
