// Run against a configured, simulated Desk in the dev browser console:
// await (await import('/dev/desk-focus-check.js')).checkDeskFocus()
// Uses no scan, link, or configuration commands.
export async function checkDeskFocus() {
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const settle = () => new Promise((resolve) => setTimeout(resolve, 1200));
  const input = document.querySelector('#ticket-code');
  const simulator = document.querySelector('.simulator > button');
  assert(input && simulator, 'Open a simulated Desk before running this check.');
  assert(!document.querySelector('dialog[open]'), 'Close dialogs before running this check.');
  simulator.focus();
  await settle();
  assert(document.activeElement === input, 'Desk must reclaim scanner focus outside dialogs.');
  simulator.click();
  const dialog = document.querySelector('.simulator-dialog');
  const uid = dialog.querySelector('#sim-uid');
  uid.focus();
  await settle();
  assert(dialog.open && document.activeElement === uid, 'Dialog input must keep focus.');
  dialog.close();
  await settle();
  assert(document.activeElement === input, 'Closing simulator must restore scanner focus.');
  window.dispatchEvent(new Event('focus'));
  await settle();
  assert(document.activeElement === input, 'Returning to app must restore scanner focus.');
  return 'Desk focus: 4 checks passed';
}
