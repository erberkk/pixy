// Mail: the inbox watcher (watcher), the one-or-two lines shown for a message
// (summary), and the once-a-day card (brief).
//
// The Google API client itself lives in google/gmail.rs — this module is the
// part with opinions in it: what counts as new, what is worth announcing, and
// what a message should say when no model is available to compress it.
pub mod brief;
pub mod summary;
pub mod watcher;
