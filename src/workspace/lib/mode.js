// Which of the Workspace window's three modes is showing. Kept in its own
// module (importing nothing) so both the shell and the individual modes can
// read it without importing each other — the memory graph's animation loop in
// particular needs to bail out as soon as the user leaves Memory mode.
let currentMode = "notes";

export const getMode = () => currentMode;

export function setModeState(mode) {
  currentMode = mode;
}
