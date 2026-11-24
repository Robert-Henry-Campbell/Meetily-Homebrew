import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { button, click, flush, handlers, named, reset, text, toasts } from './harness';

const { ClaudeCard } = await import('../../src/components/connections/ClaudeCard');
let root: ReactTestRenderer;
const mount = async () => { await act(async () => { root = create(<ClaudeCard />); }); await flush(); };
const field = (label: string) => root.root.findByProps({ 'aria-label': label });
const NONE = { keySaved: false, keyHint: null, selected: false, model: 'claude-opus-5-5' };
const SAVED = { keySaved: true, keyHint: '…WXYZ', selected: true, model: 'claude-opus-5-5' };
const MODELS = [{ id: 'claude-opus-5-5', display_name: 'Claude Opus 5.5' }, { id: 'claude-sonnet-5-5', display_name: 'Claude Sonnet 5.5' }];

beforeEach(() => reset({ claude_status: () => NONE, claude_models: () => MODELS, open_external_url: () => undefined }));
afterEach(async () => { await act(async () => root?.unmount()); });

test('a new key is checked with a real request before it is saved, with Opus 5.5 as the default model', async () => {
  handlers.claude_check_key = () => ({ ok: true, message: 'The key works with claude-opus-5-5.' });
  handlers.claude_connect = () => SAVED;
  await mount();
  expect(text(root)).toContain('Not connected');
  expect(field('Claude model').props.value).toBe('claude-opus-5-5');
  expect(button(root, 'Check and save')!.props.disabled).toBe(true);

  await act(async () => field('Claude API key').props.onChange({ target: { value: '  sk-ant-api03-new  ' } }));
  expect(field('Claude API key').props.type).toBe('password');
  await click(root, 'Check and save');

  expect(named('claude_check_key')[0][1]).toEqual({ apiKey: 'sk-ant-api03-new', model: 'claude-opus-5-5' });
  expect(named('claude_connect')[0][1]).toEqual({ apiKey: 'sk-ant-api03-new', model: 'claude-opus-5-5' });
  const shown = text(root);
  expect(shown).toContain('In use');
  expect(shown).toContain('The key works with claude-opus-5-5.');
  // The field is emptied; the saved key is only hinted at from now on.
  expect(field('Claude API key').props.value).toBe('');
  expect(field('Claude API key').props.placeholder).toContain('…WXYZ');
  expect(toasts.some(([kind, title]) => kind === 'success' && title === 'Claude writes the summaries')).toBe(true);
});

test('a key that Claude rejects is not saved, and the reason is shown in plain words', async () => {
  handlers.claude_check_key = () => ({ ok: false, message: 'Claude rejected the API key. Check the key in Settings.' });
  handlers.claude_connect = () => SAVED;
  await mount();
  await act(async () => field('Claude API key').props.onChange({ target: { value: 'sk-ant-wrong' } }));
  await click(root, 'Check and save');
  expect(text(root)).toContain('Claude rejected the API key.');
  expect(named('claude_connect')).toHaveLength(0);
  expect(text(root)).toContain('Not connected');
  // The typed key stays so it can be corrected.
  expect(field('Claude API key').props.value).toBe('sk-ant-wrong');
});

test('a saved key can be checked again and used with another model without typing it', async () => {
  reset({
    claude_status: () => ({ ...SAVED, selected: false }),
    claude_models: () => MODELS,
    claude_check_key: () => ({ ok: true, message: 'The key works with claude-sonnet-5-5.' }),
    claude_connect: () => ({ ...SAVED, model: 'claude-sonnet-5-5' }),
  });
  await mount();
  expect(text(root)).toContain('Key saved, not in use');
  await act(async () => field('Claude model').props.onChange({ target: { value: 'claude-sonnet-5-5' } }));
  await click(root, 'Check and use');
  // No key leaves the webview: the native side uses the stored one.
  expect(named('claude_check_key')[0][1]).toEqual({ apiKey: null, model: 'claude-sonnet-5-5' });
  expect(named('claude_connect')[0][1]).toEqual({ apiKey: null, model: 'claude-sonnet-5-5' });
  expect(text(root)).toContain('In use');
});

test('the card says what happens when Claude cannot be used, and removes the key after confirmation', async () => {
  reset({ claude_status: () => SAVED, claude_models: () => { throw new Error('offline'); }, claude_remove_key: () => NONE });
  await mount();
  expect(text(root)).toContain('the local model writes the summary and the meeting says so');
  // The model in use is offered even when the list could not be fetched.
  expect(root.root.findAllByType('option').map((option) => option.props.value)).toEqual(['claude-opus-5-5']);

  await click(root, 'Remove the key');
  expect(named('claude_remove_key')).toHaveLength(0);
  await click(root, 'confirm:Remove');
  expect(named('claude_remove_key')).toHaveLength(1);
  expect(text(root)).toContain('Not connected');
});
