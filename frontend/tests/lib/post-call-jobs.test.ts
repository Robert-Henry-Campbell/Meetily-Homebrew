import { expect, test } from 'bun:test';
import { parseParticipantNames, singleGuestRename } from '../../src/lib/post-call-jobs';

test('names are split on commas, semicolons and new lines, trimmed and de-duplicated in order', () => {
  expect(parseParticipantNames(' Alice  Smith, bob;\nALICE SMITH ,, Carol ')).toEqual(['Alice Smith', 'bob', 'Carol']);
  expect(parseParticipantNames('   ')).toEqual([]);
});

test('one name and one unnamed remote voice is the only case that is renamed', () => {
  expect(singleGuestRename(['Alice'], ['You', 'Speaker 1'])).toEqual({ from: 'Speaker 1', to: 'Alice' });
  expect(singleGuestRename(['Alice'], ['You', 'Guest'])).toEqual({ from: 'Guest', to: 'Alice' });
  // Combined labels count their parts once.
  expect(singleGuestRename(['Alice'], ['You', 'You + Speaker 1', 'Speaker 1'])).toEqual({ from: 'Speaker 1', to: 'Alice' });
});

test('anything ambiguous is left to the user', () => {
  expect(singleGuestRename([], ['You', 'Speaker 1'])).toBeNull();
  expect(singleGuestRename(['Alice', 'Bob'], ['You', 'Speaker 1'])).toBeNull();
  expect(singleGuestRename(['Alice'], ['You', 'Speaker 1', 'Speaker 2'])).toBeNull();
  expect(singleGuestRename(['Alice'], ['You'])).toBeNull();
  // A voice that already has a name is not overwritten.
  expect(singleGuestRename(['Alice'], ['You', 'Bob'])).toBeNull();
});
