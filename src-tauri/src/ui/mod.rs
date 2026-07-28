// Desktop shell surfaces: window show/hide/positioning (windows), the system
// tray icon and its menu (tray), and the click-through hit-testing that lets
// the transparent mascot window pass clicks to whatever is behind it
// (clickthrough).
pub mod clickthrough;
pub mod tray;
pub mod windows;
