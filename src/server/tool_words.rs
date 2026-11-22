//! What a tool does, in the words of an approval ask (33g): "Concierge wants to run a command".
//!
//! Keep these in step with `frontend/src/toolLabel.ts` `VERBS`, form 1 (the "wants to" form), so
//! the in-app card, the phone push and Telegram say the same thing. Only built-in tools get words
//! here: an MCP tool keeps naming itself and its server, as before.

/// The plain words after "<bot> wants to" for built-in tool `tool` (matched case-insensitively,
/// like toolLabel.ts): `Bash` → "run a command". `None` for an MCP tool (a `server`, or an
/// `mcp__<server>__<tool>` name) and for a tool kwant has no words for: the caller names it.
pub fn action(tool: &str, server: Option<&str>) -> Option<&'static str> {
    if server.is_some_and(|s| !s.trim().is_empty()) || tool.starts_with("mcp__") {
        return None;
    }
    Some(match tool.trim().to_ascii_lowercase().as_str() {
        "bash" => "run a command",
        "execute_shell" => "run a command on your computer",
        "read" | "read_file" => "read a file",
        "write" | "write_file" => "write a file",
        "edit" | "multiedit" | "edit_file" => "change a file",
        "webfetch" | "web_fetch" => "open a page",
        "websearch" | "web_search" => "search the web",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::action;

    #[test]
    fn built_in_tools_read_as_what_they_do() {
        for (tool, words) in [
            ("Bash", "run a command"),
            ("bash", "run a command"),
            ("execute_shell", "run a command on your computer"),
            ("Read", "read a file"),
            ("Write", "write a file"),
            ("write_file", "write a file"),
            ("Edit", "change a file"),
            ("MultiEdit", "change a file"),
            ("edit_file", "change a file"),
            ("WebFetch", "open a page"),
            ("web_fetch", "open a page"),
            ("WebSearch", "search the web"),
            ("websearch", "search the web"),
        ] {
            assert_eq!(action(tool, None), Some(words), "{tool}");
            assert_eq!(action(tool, Some("")), Some(words), "{tool}: a blank server is no server");
        }
    }

    #[test]
    fn mcp_and_unknown_tools_keep_their_names() {
        assert_eq!(action("Foo", None), None);
        assert_eq!(action("", None), None);
        assert_eq!(action("create_issue", Some("github")), None);
        // A shell tool on an MCP server is that server's, not claude's Bash
        assert_eq!(action("bash", Some("shell")), None);
        assert_eq!(action("mcp__shell__bash", None), None);
    }
}
