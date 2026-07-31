// Calendar: the "something is about to start" watcher.
//
// Reading and parsing events lives in google/calendar.rs; this module only
// decides when one is worth interrupting the user for, and remembers which
// occurrences it has already mentioned.
pub mod watcher;
