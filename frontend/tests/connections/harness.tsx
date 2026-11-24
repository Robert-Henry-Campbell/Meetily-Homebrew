/**
 * Shared fixtures for the Connections tests: mocked IPC, native events, toasts
 * and the dialog primitives (portals need a DOM that the test renderer lacks).
 */
import React from 'react';
import { act, type ReactTestRenderer } from 'react-test-renderer';
import { mock } from 'bun:test';

export const calls: Array<[string, any]> = [];
export const toasts: Array<[string, string, any]> = [];
export const nativeListeners = new Map<string, (event: { payload: any }) => void>();
export const pushed: string[] = [];
export const handlers: Record<string, (args: any) => unknown> = {};

Object.assign(globalThis, { React });
mock.module('@tauri-apps/api/core', () => ({ invoke: async (name: string, args: any) => {
  calls.push([name, args]);
  if (!(name in handlers)) throw new Error(`Unexpected command ${name}`);
  return handlers[name](args);
} }));
mock.module('@tauri-apps/api/event', () => ({ listen: async (name: string, fn: any) => {
  nativeListeners.set(name, fn);
  return () => nativeListeners.delete(name);
} }));
const toast = (kind: string) => (title: string, options?: any) => { toasts.push([kind, title, options]); };
mock.module('sonner', () => ({ toast: { success: toast('success'), error: toast('error'), info: toast('info'), warning: toast('warning') } }));
mock.module('next/navigation', () => ({ useRouter: () => ({ push: (route: string) => pushed.push(route) }) }));
mock.module('next/link', () => ({ default: ({ children, href }: any) => <a href={href}>{children}</a> }));
mock.module('@/components/ui/button', () => ({ Button: ({ asChild, loading, variant, size, ...props }: any) => <button {...props} /> }));
mock.module('@/components/ui/dialog', () => ({
  Dialog: ({ open, children }: any) => (open ? <div role="dialog">{children}</div> : null),
  DialogContent: ({ children }: any) => <div>{children}</div>,
  DialogHeader: ({ children }: any) => <div>{children}</div>,
  DialogFooter: ({ children }: any) => <div>{children}</div>,
  DialogTitle: ({ children }: any) => <h2>{children}</h2>,
  DialogDescription: ({ children }: any) => <p>{children}</p>,
}));
// Always open: the tests look at what a popover offers, not at its motion.
mock.module('@/components/ui/popover', () => ({
  Popover: ({ children }: any) => <div>{children}</div>,
  PopoverTrigger: ({ children }: any) => children,
  PopoverContent: ({ children }: any) => <div role="region">{children}</div>,
}));
mock.module('@/components/ui/switch', () => ({
  Switch: ({ checked, onCheckedChange, className, ...props }: any) => (
    <button role="switch" aria-checked={checked} onClick={() => onCheckedChange(!checked)} {...props} />
  ),
}));
mock.module('@/components/ui/confirm-dialog', () => ({
  ConfirmDialog: ({ open, title, description, confirmLabel, onConfirm, onOpenChange }: any) => (open ? (
    <div role="alertdialog" data-title={title}>
      <p>{description}</p>
      <button onClick={async () => { await onConfirm(); onOpenChange(false); }}>{`confirm:${confirmLabel}`}</button>
    </div>
  ) : null),
}));

export const NOT_CONNECTED = {
  clientConfigured: false, clientIdHint: null, connected: false, accountEmail: null,
  driveGranted: false, calendarGranted: false, needsAttention: null,
};
export const CLIENT_ONLY = { ...NOT_CONNECTED, clientConfigured: true, clientIdHint: '12345678….apps.googleusercontent.com' };
export const CONNECTED = { ...CLIENT_ONLY, connected: true, accountEmail: 'person@example.com', driveGranted: true, calendarGranted: true };

export function reset(next: Record<string, (args: any) => unknown>) {
  calls.length = 0; toasts.length = 0; pushed.length = 0; nativeListeners.clear();
  for (const key of Object.keys(handlers)) delete handlers[key];
  Object.assign(handlers, next);
}

/** Every piece of text the tree shows, in order. */
export function text(root: ReactTestRenderer): string {
  const walk = (node: any): string => {
    if (node === null || node === undefined || typeof node === 'boolean') return '';
    if (typeof node === 'string' || typeof node === 'number') return String(node);
    if (Array.isArray(node)) return node.map(walk).join(' ');
    return walk(node.children);
  };
  return walk(root.toJSON()).replace(/\s+/g, ' ');
}

export const button = (root: ReactTestRenderer, label: string) =>
  root.root.findAllByType('button').find((node) => {
    const parts = (Array.isArray(node.props.children) ? node.props.children : [node.props.children]).flat(Infinity);
    return parts.some((part: unknown) => typeof part === 'string' && part.trim() === label);
  });

export const click = (root: ReactTestRenderer, label: string) => act(async () => {
  const target = button(root, label);
  if (!target) throw new Error(`No button "${label}"`);
  await target.props.onClick();
});

export const named = (name: string) => calls.filter(([command]) => command === name);
export const flush = () => act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });
