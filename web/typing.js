// Characters belong to the viewer layout; shortcut positions belong to keymap.js.
export function typedKey(e, { macViewer, commandHeld = e.metaKey }) {
  const altGraph = e.getModifierState?.('AltGraph') === true;
  if (e.isComposing || e.keyCode === 229 || (commandHeld && !altGraph) || e.metaKey) return undefined;
  if (e.ctrlKey && !altGraph) return undefined;
  if (e.altKey && !altGraph && !macViewer) return undefined;
  // Named keys (Dead, Enter, Unidentified, etc.) are not committed characters.
  if (!e.key || [...e.key].length !== 1 || /[\u0000-\u001f\u007f]/u.test(e.key)) return undefined;
  return e.key;
}
