//! Tool-call **view steps** (aionui-style).
//!
//! When a CLI reports a `tool_call`/`tool_use` event the gateway projects a short
//! human-readable view line into the reasoning channel (`ReasoningDelta`) so
//! clients (AionUI / OpenAI-compatible `reasoning_content`) can show what the
//! agent is doing (e.g. `🔧 grep …`, `📝 edit …`, `bash …`).
//!
//! This is a faithful projection of **CLI-reported** tool events — it is not
//! synthetic gateway activity ("Menyiapkan agen" / "Model sedang bekerja").
//! No reasoning is fabricated; only observed tool calls are rendered.
//!
//! Editing operations (create/write/edit/patch/delete/mkdir) are the primary
//! focus: they render with an explicit `+N`/`-N` change summary so the user can
//! see *what the agent changed*, not just that some tool ran.

/// Tool families that mutate the workspace.
///
/// The UI highlights these because the user cares most about what the agent is
/// creating, editing, or deleting — reads/searches are secondary context.
pub fn is_edit_operation(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "write"
            | "create"
            | "write_file"
            | "create_file"
            | "new_file"
            | "file_write"
            | "write_to_file"
            | "edit"
            | "replace"
            | "modify"
            | "patch"
            | "edit_file"
            | "multiedit"
            | "multi_edit"
            | "str_replace"
            | "str_replace_editor"
            | "apply_patch"
            | "insert"
            | "append"
            | "notebook_edit"
            | "delete"
            | "remove"
            | "rm"
            | "unlink"
            | "delete_file"
            | "rm_file"
            | "mkdir"
            | "make_dir"
    )
}

/// Render a single tool call as a one-line view step.
///
/// `name` is matched case-insensitively against known tool families
/// (grep/glob/read/create/edit/write/bash/exec/cmd/run/…). Unknown names
/// fall back to a generic `tool` line so nothing is silently dropped.
pub fn format_tool_view_step(name: &str, arguments: &str) -> String {
    let name = name.trim().to_lowercase();
    let args = arguments.trim();

    // Heuristic: pull the first path-like argument when present, else show the
    // raw arguments truncated (keeps the line short and readable).
    let target = first_path_arg(arguments).unwrap_or(args);

    let (verb, icon) = match name.as_str() {
        "grep" | "rg" | "search" | "find" => ("search", "🔍"),
        "glob" | "ls" | "list" | "find_files" | "list_files" => ("list", "📁"),
        "read" | "cat" | "view" | "get_file" | "read_file" | "readfile" | "getfile" => ("read", "📄"),
        "write" | "create" | "write_file" | "create_file" | "new_file" | "file_write"
        | "write_to_file" | "writefile" | "createfile" => ("write", "📝"),
        "edit" | "replace" | "modify" | "patch" | "edit_file" | "multiedit" | "multi_edit"
        | "str_replace" | "str_replace_editor" | "apply_patch" | "insert" | "append"
        | "notebook_edit" => ("edit", "✏️"),
        "delete" | "remove" | "rm" | "unlink" | "delete_file" | "rm_file" => ("delete", "🗑️"),
        "bash" | "shell" | "exec" | "run" | "execute" | "cmd" | "command" | "execcommand"
        | "exec_command" | "shellcommand" | "shell_command" | "runcode" => ("run", "⚡"),
        "mkdir" | "make_dir" => ("mkdir", "📂"),
        _ => ("tool", "🧩"),
    };

    let mut line = if target.is_empty() {
        format!("{} {} {}", icon, verb, name)
    } else if verb == name {
        format!("{} {} → {}", icon, verb, truncate(target, 120))
    } else {
        format!("{} {} {} → {}", icon, verb, name, truncate(target, 120))
    };

    // Editing operations get a change summary so the view focuses on the actual
    // mutation (`✏️ edit → a.rs (-2 +3)`) instead of just the tool name.
    if let Some(stats) = change_stats(&name, arguments) {
        line.push_str(&stats);
    }

    line
}

/// Compute a compact `(+N)` / `(-N +M)` change summary for edit-like tools.
fn change_stats(name: &str, arguments: &str) -> Option<String> {
    match name {
        "write" | "create" | "write_file" | "create_file" | "new_file" | "file_write"
        | "write_to_file" | "append" | "insert" => {
            let added = extract_json_string_arg(arguments, &["content", "contents", "text", "newstring", "new_string"])
                .map(count_lines)
                .unwrap_or(0);
            (added > 0).then(|| format!(" (+{} lines)", added))
        }
        "edit" | "replace" | "modify" | "patch" | "edit_file" | "multiedit" | "multi_edit"
        | "str_replace" | "str_replace_editor" | "apply_patch" | "notebook_edit" => {
            let removed = extract_json_string_arg(
                arguments,
                &["oldstring", "old_string", "old", "find", "search", "original"],
            )
            .map(count_lines)
            .unwrap_or(0);
            let added = extract_json_string_arg(
                arguments,
                &["newstring", "new_string", "new", "replace", "replacement"],
            )
            .map(count_lines)
            .unwrap_or(0);
            if removed == 0 && added == 0 {
                None
            } else {
                Some(format!(" (-{} +{})", removed, added))
            }
        }
        _ => None,
    }
}

fn count_lines(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.lines().count().max(1)
    }
}

/// Extract the first JSON string value that looks like a path/argument from a
/// JSON arguments blob (best-effort, no allocation beyond the scan).
fn first_path_arg(arguments: &str) -> Option<&str> {
    // Two-pass: prefer concrete targets (path/command/url) over search terms
    // (pattern/query) so file ops show the file and run shows the command;
    // fall back to pattern/query for search tools.
    const PRIMARY: &[&str] = &[
        "path",
        "file",
        "file_path",
        "filepath",
        "filename",
        "file_name",
        "absolute_path",
        "absolutepath",
        "relative_path",
        "relativepath",
        "location",
        "dir",
        "directory",
        "url",
        "command",
        "cmd",
    ];
    const SECONDARY: &[&str] = &["pattern", "query", "name"];
    extract_json_string_arg(arguments, PRIMARY)
        .or_else(|| extract_json_string_arg(arguments, SECONDARY))
}

fn extract_json_string_arg<'a>(arguments: &'a str, keys: &[&str]) -> Option<&'a str> {
    let bytes = arguments.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'"' {
            if let Some(end) = arguments[i + 1..].find('"') {
                let key = arguments[i + 1..i + 1 + end].to_ascii_lowercase();
                if keys.iter().any(|k| *k == key) {
                    let rest = &arguments[i + 1 + end + 1..];
                    if let Some(colon) = rest.find(':') {
                        let after = rest[colon + 1..].trim_start();
                        if let Some(q) = after.find('"') {
                            let val = &after[q + 1..];
                            if let Some(qe) = val.find('"') {
                                let v = &val[..qe];
                                if !v.is_empty() {
                                    return Some(v);
                                }
                            }
                        }
                    }
                }
                i += 1 + end + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let mut out: String = chars.iter().take(max - 3).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tools_render_with_icon_and_target() {
        assert_eq!(
            format_tool_view_step("grep", r#"{"pattern":"hello","path":"src"}"#),
            "🔍 search grep → src"
        );
        assert_eq!(
            format_tool_view_step("edit", r#"{"file_path":"a.rs","old":"x","new":"y"}"#),
            "✏️ edit → a.rs (-1 +1)"
        );
        assert_eq!(
            format_tool_view_step("bash", r#"{"command":"ls -la"}"#),
            "⚡ run bash → ls -la"
        );
        assert_eq!(
            format_tool_view_step("create", r#"{"path":"new.txt","content":"hi"}"#),
            "📝 write create → new.txt (+1 lines)"
        );
    }

    #[test]
    fn opencode_camel_case_filepath_is_target() {
        // OpenCode tool_use input uses camelCase `filePath`.
        assert_eq!(
            format_tool_view_step("write", "{\"filePath\":\"src/main.rs\",\"content\":\"a\nb\"}"),
            "📝 write → src/main.rs (+2 lines)"
        );
        assert_eq!(
            format_tool_view_step(
                "multiedit",
                "{\"filePath\":\"src/lib.rs\",\"oldString\":\"a\",\"newString\":\"b\nc\"}"
            ),
            "✏️ edit multiedit → src/lib.rs (-1 +2)"
        );
    }

    #[test]
    fn edit_operations_are_flagged() {
        for name in [
            "create", "write", "edit", "patch", "delete", "mkdir", "multiedit",
            "str_replace_editor", "apply_patch",
        ] {
            assert!(is_edit_operation(name), "{name} should be an edit op");
        }
        for name in ["read", "grep", "glob", "bash", "webfetch"] {
            assert!(!is_edit_operation(name), "{name} should not be an edit op");
        }
    }

    #[test]
    fn unknown_tool_falls_back_to_generic() {
        let out = format_tool_view_step("dll", r#"{"path":"x.dll"}"#);
        assert!(out.contains("dll"));
        assert!(out.contains("🧩"));
    }

    #[test]
    fn empty_arguments_still_render() {
        let out = format_tool_view_step("read", "{}");
        assert!(out.contains("read"));
    }
}
