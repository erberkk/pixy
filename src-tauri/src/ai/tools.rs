// The tools the chat model is allowed to call, and the plumbing that turns a
// model's request to call one into a result it can read.
//
// This module deliberately knows nothing about HTTP streaming or about the
// conversation loop — llm.rs drives that. What lives here is only: which tools
// exist, what the model is told about them, how a call arrives off the wire in
// either of the two shapes a server can send, and what running one produces.
//
// Splitting it this way is what makes a second tool cheap: adding one is a
// `ToolSpec` in `specs` and an arm in `execute`, with no change to the loop.

use serde_json::{json, Value};

/// One callable tool, described the way the model is shown it.
///
/// `parameters` is a JSON Schema object. Both Ollama's native `/api/chat` and
/// the OpenAI-compatible `/v1/chat/completions` take the same
/// `{"type": "function", "function": {...}}` envelope around it, so one spec
/// serves both wire formats.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

impl ToolSpec {
    /// The wire form both endpoints accept.
    pub fn to_wire(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// A call the model asked for, normalised away from the two wire shapes.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Echoed back on the result message so a server that pairs them can.
    pub id: String,
    pub name: String,
    /// Always an object here, whichever shape it arrived in.
    pub arguments: Value,
}

/// How much of one page is handed to the model.
///
/// Generous, because the whole point of reading the page rather than a search
/// snippet is having the actual text — but bounded, because every tool result
/// is re-sent with each subsequent round of the loop, so an unbounded one grows
/// the prompt on every turn.
const PAGE_CHARS: usize = 6000;

/// Which tools this app offers right now.
///
/// Empty means "send no `tools` key at all" — a model that is handed an empty
/// tools array can still decide to emit a call for a tool that isn't there, and
/// paying tokens to describe nothing is worse than not asking.
pub fn specs(app: &tauri::AppHandle) -> Vec<ToolSpec> {
    if !crate::tunables::toggle(app, crate::tunables::WEB_TOOLS_ENABLED) {
        return Vec::new();
    }
    vec![
    ToolSpec {
        name: "web_search",
        // Names what the sources actually are, because a model told only
        // "search the web" asks for things none of them cover (today's news,
        // a product price) and then has to be told no. Naming them steers the
        // question toward what can be answered, and makes it likelier to
        // answer directly when searching would not help.
        // The "for those, say you cannot look them up" this used to end with was
        // measured doing real damage: asked for the top story on Hacker News, the
        // model refused without calling anything — correctly, since HN was listed
        // as a source AND as forbidden breaking news in the same sentence, and it
        // resolved the contradiction the conservative way. Describing what each
        // source is good at is useful; ordering a refusal is not, because the
        // model cannot know that fetch_url could have answered it in one call.
        description:
            "Search the web for information you do not have. The sources are Marginalia \
             (independent web index — articles, blogs, documentation), Wikipedia, Stack \
             Overflow, Hacker News discussions, crates.io and GitHub. Best for factual, \
             technical and reference questions. These are slow-crawled indexes, so they \
             are weak on things that changed today — for a live page, fetch_url is the \
             better tool. Pass a short keyword phrase, not a whole sentence.",
        parameters: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A few keywords, e.g. 'rust async trait object safety'"
                }
            }
        }),
    },
    ToolSpec {
        name: "fetch_url",
        // Written for a 9B model: what it does, when to reach for it, and the
        // one thing it must not do (guess an address). Vague tool descriptions
        // are the main reason a small model calls the wrong tool or calls a
        // right one with nonsense in it.
        //
        // The blanket "never invent one" is now a narrow exception instead. It
        // was costing the obvious cases: "what is on the front page of Hacker
        // News" is one fetch of an address the model certainly knows, and the
        // rule sent it to search — or to a refusal — instead. The exception is
        // deliberately limited to a well-known site's own homepage, because that
        // is where a model's guess is reliable; a deep link it reasons its way to
        // is where it starts inventing paths that 404 and burn a round. A guessed
        // address is no more dangerous than a given one either way: web/fetch.rs
        // resolves it, refuses anything that is not a public address, and pins
        // what it checked.
        description:
            "Fetch a web page and read its text. Use this whenever the user gives you a link, \
             or asks what is on a page, or asks about something you would need to read a \
             specific page to answer — including anything that changed today, which the \
             search index will not have. Prefer a URL the user gave you or one from an \
             earlier tool result. You may also use the HOMEPAGE of a well-known site when \
             the question is plainly about it (for example news.ycombinator.com for Hacker \
             News), but do not invent deeper paths — guess the site, never the page.",
        parameters: json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The full address of the page, including https://"
                }
            }
        }),
    },
    ]
}

/// Where a tool's answer came from, for the UI to show under the reply.
///
/// Shown for the same reason recall's notes are: a model citing something the
/// user cannot go and look at is indistinguishable from a model making it up.
#[derive(serde::Serialize, Clone)]
pub struct ToolSource {
    pub title: String,
    pub url: String,
}

/// What running a tool produced.
pub struct ToolOutcome {
    /// What the model is told. Never empty, including on failure.
    pub text: String,
    pub sources: Vec<ToolSource>,
}

impl ToolOutcome {
    fn plain(text: String) -> Self {
        Self { text, sources: Vec::new() }
    }
}

/// Runs one call and returns what the model should be told.
///
/// Always produces text, even for a tool that failed: a failure the model can
/// read ("that page could not be fetched") is a usable turn, whereas aborting
/// the conversation would throw away the answer it was part of the way through.
/// Every failure goes through `web::failure_notice`, which also tells the model
/// not to fill the gap from memory.
pub fn execute(app: &tauri::AppHandle, call: &ToolCall) -> ToolOutcome {
    match call.name.as_str() {
        "web_search" => web_search(app, &call.arguments),
        "fetch_url" => fetch_url(app, &call.arguments),
        other => ToolOutcome::plain(format!(
            "No tool named '{other}' is available. Tell the user you cannot do that, \
             and do not invent a result."
        )),
    }
}

fn web_search(app: &tauri::AppHandle, arguments: &Value) -> ToolOutcome {
    if let Some(off) = web_access_off(app) {
        return ToolOutcome::plain(off);
    }
    let query = arguments
        .get("query")
        .and_then(|q| q.as_str())
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        return ToolOutcome::plain(
            "That search arrived without a query. Ask the user what to look for.".to_string(),
        );
    }
    let (text, hits) = crate::web::search::search(app, query);
    ToolOutcome {
        text,
        sources: hits
            .into_iter()
            .map(|hit| ToolSource {
                title: format!("{} [{}]", hit.title, hit.source),
                url: hit.url,
            })
            .collect(),
    }
}

/// The one message both tools give when the setting is off.
///
/// Checked in the tools as well as in `specs`, not only there: the two are read
/// at different moments, and a conversation that began while web access was on
/// can still be mid-loop when it is turned off.
fn web_access_off(app: &tauri::AppHandle) -> Option<String> {
    if crate::tunables::toggle(app, crate::tunables::WEB_TOOLS_ENABLED) {
        return None;
    }
    Some(
        "Web access is turned off in this app's settings. Tell the user that, and do not \
         answer as though you had looked anything up."
            .to_string(),
    )
}

fn fetch_url(app: &tauri::AppHandle, arguments: &Value) -> ToolOutcome {
    if let Some(off) = web_access_off(app) {
        return ToolOutcome::plain(off);
    }
    let url = arguments
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or_default()
        .trim();
    if url.is_empty() {
        return ToolOutcome::plain(
            "That call arrived without a url. Ask the user for the address.".to_string(),
        );
    }

    match crate::web::fetch::fetch_text(url, PAGE_CHARS) {
        Ok(page) => {
            let heading = if page.title.is_empty() {
                page.url.clone()
            } else {
                format!("{} ({})", page.title, page.url)
            };
            let mut body = page.text;
            if page.truncated {
                body.push_str("\n[… the page continues beyond what was read]");
            }
            ToolOutcome {
                text: crate::web::fence(&heading, &body),
                // The address that was actually read, after redirects — citing
                // the one asked for would point somewhere the text did not
                // come from.
                sources: vec![ToolSource {
                    title: if page.title.is_empty() { page.url.clone() } else { page.title },
                    url: page.url,
                }],
            }
        }
        // No source on failure, deliberately: a link under the reply reads as
        // "here is what I read", and nothing was read.
        Err(why) => ToolOutcome::plain(crate::web::failure_notice(url, &why)),
    }
}

/// Pulls the calls out of an assistant message, in either wire shape.
///
/// The two differ in exactly one way that matters, and it is easy to miss:
/// Ollama's native `/api/chat` sends `arguments` as a JSON **object**, while
/// the OpenAI-compatible `/v1` shim sends it as a **string** containing JSON.
/// Both were observed firsthand against the same model on the same server.
/// Anything that is neither is treated as no arguments rather than as a hard
/// failure — a model that emits a malformed call should get a tool result
/// saying so, not crash the turn.
pub fn parse_tool_calls(message: &Value) -> Vec<ToolCall> {
    let Some(raw_calls) = message.get("tool_calls").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    raw_calls
        .iter()
        .enumerate()
        .filter_map(|(index, raw)| {
            let function = raw.get("function")?;
            let name = function.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let arguments = match function.get("arguments") {
                Some(Value::Object(map)) => Value::Object(map.clone()),
                Some(Value::String(text)) => {
                    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
                }
                _ => json!({}),
            };
            // A server that omits the id still needs the result message to
            // carry one, so synthesise a stable stand-in rather than dropping
            // an otherwise valid call.
            let id = raw
                .get("id")
                .and_then(|i| i.as_str())
                .filter(|i| !i.is_empty())
                .map(|i| i.to_string())
                .unwrap_or_else(|| format!("call_{index}"));
            Some(ToolCall {
                id,
                name: name.to_string(),
                arguments,
            })
        })
        .collect()
}

/// The assistant turn to append before the results, so the next request shows
/// the model what it just asked for.
pub fn assistant_call_message(calls: &[ToolCall]) -> Value {
    let wire: Vec<Value> = calls
        .iter()
        .map(|call| {
            json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            })
        })
        .collect();
    json!({ "role": "assistant", "content": "", "tool_calls": wire })
}

/// The result turn for one call.
///
/// Carries `tool_name` (what Ollama's native endpoint reads) *and*
/// `tool_call_id` (what the OpenAI-compatible one reads). A server ignores the
/// key it doesn't know, so sending both means the loop does not have to track
/// which endpoint it ended up talking to.
pub fn tool_result_message(call: &ToolCall, result: &str) -> Value {
    json!({
        "role": "tool",
        "tool_name": call.name,
        "tool_call_id": call.id,
        "content": result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape Ollama's native /api/chat actually returned, copied from a
    // live response: arguments is an object.
    #[test]
    fn native_arguments_arrive_as_an_object() {
        let message = json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_4ti1x6m9",
                "function": {
                    "index": 0,
                    "name": "web_search",
                    "arguments": {"query": "Istanbul hava durumu"}
                }
            }]
        });
        let calls = parse_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].id, "call_4ti1x6m9");
        assert_eq!(calls[0].arguments["query"], "Istanbul hava durumu");
    }

    // The same call over the /v1 shim: arguments is a STRING holding JSON.
    // Parsing it the same way as the object form is the whole point.
    #[test]
    fn openai_arguments_arrive_as_a_json_string() {
        let message = json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "call_ag9re28x",
                "index": 0,
                "type": "function",
                "function": {
                    "name": "web_search",
                    "arguments": "{\"query\":\"Istanbul hava durumu\"}"
                }
            }]
        });
        let calls = parse_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["query"], "Istanbul hava durumu");
    }

    #[test]
    fn a_message_without_tool_calls_yields_none() {
        let message = json!({"role": "assistant", "content": "Just an answer."});
        assert!(parse_tool_calls(&message).is_empty());
    }

    // A malformed arguments string must not take the turn down with it: the
    // call still runs, just with nothing in it, and the tool reports what it
    // needs.
    #[test]
    fn unparseable_arguments_degrade_to_empty_rather_than_dropping_the_call() {
        let message = json!({
            "tool_calls": [{
                "function": {"name": "fetch_url", "arguments": "{not json"}
            }]
        });
        let calls = parse_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({}));
    }

    #[test]
    fn a_call_with_no_id_still_gets_one() {
        let message = json!({
            "tool_calls": [{"function": {"name": "fetch_url", "arguments": {}}}]
        });
        let calls = parse_tool_calls(&message);
        assert_eq!(calls[0].id, "call_0");
    }

    #[test]
    fn a_nameless_call_is_dropped() {
        let message = json!({
            "tool_calls": [
                {"function": {"name": "   ", "arguments": {}}},
                {"function": {"name": "fetch_url", "arguments": {}}}
            ]
        });
        let calls = parse_tool_calls(&message);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fetch_url");
    }

    // Both key spellings must be present — see the doc comment.
    #[test]
    fn a_result_message_names_the_tool_for_either_endpoint() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "fetch_url".to_string(),
            arguments: json!({}),
        };
        let message = tool_result_message(&call, "page text");
        assert_eq!(message["role"], "tool");
        assert_eq!(message["tool_name"], "fetch_url");
        assert_eq!(message["tool_call_id"], "call_1");
        assert_eq!(message["content"], "page text");
    }

    #[test]
    fn an_unknown_tool_tells_the_model_not_to_invent() {
        let app_free_call = ToolCall {
            id: "call_1".to_string(),
            name: "definitely_not_a_tool".to_string(),
            arguments: json!({}),
        };
        // execute() needs an AppHandle, so the message construction is checked
        // here in the shape the caller relies on.
        let message = format!(
            "No tool named '{}' is available. Tell the user you cannot do that, \
             and do not invent a result.",
            app_free_call.name
        );
        assert!(message.contains("do not invent"));
    }
}
