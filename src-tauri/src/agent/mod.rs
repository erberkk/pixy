// Claude Code agent integration: the local HTTP server that receives the
// agent's hook events and routes permission prompts back to it.
//
// There is no terminal module any more. This app used to run Claude inside a
// pool of its own PTY-backed windows and read what it was doing off the
// rendered screen; the hooks replaced that, and they fire wherever the user
// actually runs Claude — so the pool was 16 cmd.exe plus 16 conhost.exe at
// startup buying nothing. See SETUP.md's design-decisions section.
pub mod server;
