// Local AI model integration: the LLM connection itself (llm), the chat
// conversations built on top of it (chat), the speech-to-text /
// text-to-speech server settings (speech), and the voice assistant that
// chains all three together (voice). Starting/stopping any of those local
// servers is shared, model-agnostic plumbing and lives in process.
//
// `recall` is the cross-conversation memory: a derived search index over the
// chats, so a question can reach something decided in an earlier conversation —
// typed or spoken.
pub mod chat;
pub mod llm;
pub mod process;
pub mod recall;
pub mod speech;
pub mod voice;
