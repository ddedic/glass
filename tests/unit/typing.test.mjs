import { test } from 'node:test';
import assert from 'node:assert/strict';
import { typedKey } from '../../web/typing.js';

test('viewer characters include space, punctuation, accents and supplementary Unicode', () => {
  for (const key of ['a', 'A', ' ', '@', 'č', '😀']) {
    assert.equal(typedKey({ key }, { macViewer: true }), key);
  }
  assert.equal(typedKey({ key: '@', altKey: true }, { macViewer: true }), '@');
  assert.equal(typedKey({ key: '@', altKey: true }, { macViewer: false }), undefined);
});

test('named keys, control characters and composition intermediates are not text', () => {
  for (const key of ['Dead', 'Unidentified', 'Enter', 'Tab', 'ArrowLeft', '', '\n', '\t', '\x7f']) {
    assert.equal(typedKey({ key }, { macViewer: true }), undefined);
  }
  assert.equal(typedKey({ key: 'a', isComposing: true }, { macViewer: true }), undefined);
  assert.equal(typedKey({ key: 'a', keyCode: 229 }, { macViewer: true }), undefined);
});

test('shortcuts stay physical but explicit AltGraph can include synthetic Ctrl', () => {
  assert.equal(typedKey({ key: 'c', ctrlKey: true }, { macViewer: true }), undefined);
  assert.equal(typedKey({ key: 'c', metaKey: true }, { macViewer: true }), undefined);
  const altGr = { key: '@', ctrlKey: true, altKey: true, getModifierState: (key) => key === 'AltGraph' };
  assert.equal(typedKey(altGr, { macViewer: false, commandHeld: true }), '@');
  assert.equal(typedKey({ key: 'h', altKey: true }, { macViewer: true, commandHeld: true }), undefined);
});
