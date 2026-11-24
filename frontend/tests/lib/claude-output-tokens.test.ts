import { describe, expect, test } from 'bun:test';
import { claudeOutputBudget, parseClaudeOutputTokens } from '../../src/lib/claude-output-tokens';

describe('Claude output budget', () => {
  test('uses an application default, with legacy limits and conservative unknown-model fallback', () => {
    // The default leaves room for thinking, which counts toward the limit on current models.
    expect(claudeOutputBudget('claude-sonnet-4-5-20250929')).toEqual({ maximum: 64000, defaultTokens: 32000 });
    expect(claudeOutputBudget('claude-opus-5-5')).toEqual({ maximum: 64000, defaultTokens: 32000 });
    expect(claudeOutputBudget('claude-haiku-5-5').defaultTokens).toBe(32000);
    expect(claudeOutputBudget('claude-3-sonnet-20240229').defaultTokens).toBe(4096);
    expect(claudeOutputBudget('claude-3-5-sonnet-latest').defaultTokens).toBe(8192);
    expect(claudeOutputBudget('claude-unknown').maximum).toBe(8192);
  });
  test('distinguishes blank from invalid input instead of truncating parseInt input', () => {
    const model = 'claude-sonnet-4-5';
    expect(parseClaudeOutputTokens('  ', model)).toBeNull();
    expect(parseClaudeOutputTokens('32000', model)).toBe(32000);
    for (const value of ['0', '-1', '1.5', '1e3', '12abc', '64001', '4294967296', 'NaN']) {
      expect(() => parseClaudeOutputTokens(value, model)).toThrow();
    }
    expect(() => parseClaudeOutputTokens('8192', 'claude-3-opus')).toThrow();
  });
});
