// Claude Code agent integration: the PTY-backed terminal sessions (terminal)
// and the local HTTP server that receives the agent's hook events and routes
// permission prompts back to it (server).
pub mod server;
pub mod terminal;
