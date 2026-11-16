//! Atlassian Document Format, the JSON that Jira v3 uses for descriptions and
//! comments, converted to plain text for the FTS index and to Markdown for display.

use serde_json::Value;

/// The text of every text node, one block per line.
pub(crate) fn to_plain_text(doc: Option<&Value>) -> String {
    let mut out = String::new();
    if let Some(doc) = doc {
        push_plain_text(doc, &mut out);
    }
    out.trim().to_owned()
}

fn push_plain_text(node: &Value, out: &mut String) {
    match node["content"].as_array() {
        Some(children) => {
            for child in children {
                push_plain_text(child, out);
            }
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        None if node_type(node) == "hardBreak" => out.push('\n'),
        None => out.push_str(leaf_text(node)),
    }
}

/// Markdown for `TextView`. `issue_url` is where a `media` link leads: an
/// attachment cannot be shown or fetched without Jira, so the link opens the issue.
pub(crate) fn to_markdown(doc: Option<&Value>, issue_url: &str) -> String {
    doc.map(|doc| blocks(children(doc), issue_url))
        .unwrap_or_default()
}

fn node_type(node: &Value) -> &str {
    node["type"].as_str().unwrap_or_default()
}

fn children(node: &Value) -> &[Value] {
    node["content"].as_array().map_or(&[], Vec::as_slice)
}

/// `text` of a text node, `attrs.text` of a mention, an emoji or a status.
fn leaf_text(node: &Value) -> &str {
    node["text"]
        .as_str()
        .or_else(|| node["attrs"]["text"].as_str())
        .unwrap_or_default()
}

fn is_list(node: &Value) -> bool {
    matches!(node_type(node), "bulletList" | "orderedList")
}

// Inline nodes are leaves. The only leaves that stand as blocks are `media` and `rule`.
fn is_inline(node: &Value) -> bool {
    node.get("content").is_none() && !matches!(node_type(node), "media" | "rule")
}

fn blocks(nodes: &[Value], issue_url: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut rest = nodes;
    while let Some(first) = rest.first() {
        // An unknown container (a task item, a caption) holds inline nodes directly
        let run = rest.iter().take_while(|node| is_inline(node)).count();
        if run > 0 {
            out.push(inline(&rest[..run], issue_url));
            rest = &rest[run..];
        } else {
            out.push(block(first, issue_url));
            rest = &rest[1..];
        }
    }
    out.retain(|block| !block.trim().is_empty());
    out.join("\n\n")
}

fn block(node: &Value, issue_url: &str) -> String {
    let content = children(node);
    match node_type(node) {
        "paragraph" => inline(content, issue_url),
        "heading" => {
            let level = node["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 6);
            format!(
                "{} {}",
                "#".repeat(level as usize),
                inline(content, issue_url)
            )
        }
        "bulletList" => list(content, None, issue_url),
        "orderedList" => {
            let start = node["attrs"]["order"].as_u64().unwrap_or(1);
            list(content, Some(start), issue_url)
        }
        "codeBlock" => {
            let code: String = content.iter().map(leaf_text).collect();
            let fence = "`".repeat(longest_backtick_run(&code).max(2) + 1);
            let language = node["attrs"]["language"].as_str().unwrap_or_default();
            format!("{fence}{language}\n{code}\n{fence}")
        }
        "blockquote" | "panel" => prefix_lines(&blocks(content, issue_url), "> ", "> "),
        "rule" => "---".to_owned(),
        "table" => table(content, issue_url),
        "media" => media(node, issue_url),
        _ => blocks(content, issue_url),
    }
}

fn list(items: &[Value], start: Option<u64>, issue_url: &str) -> String {
    let mut lines = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let marker = match start {
            Some(start) => format!("{}. ", start + index as u64),
            None => "- ".to_owned(),
        };
        let mut body = String::new();
        for child in children(item) {
            let rendered = block(child, issue_url);
            if rendered.is_empty() {
                continue;
            }
            if !body.is_empty() {
                // A nested list stays tight under its parent line
                body.push_str(if is_list(child) { "\n" } else { "\n\n" });
            }
            body.push_str(&rendered);
        }
        lines.push(prefix_lines(&body, &marker, &" ".repeat(marker.len())));
    }
    lines.join("\n")
}

fn prefix_lines(text: &str, first: &str, rest: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 { first } else { rest };
            format!("{prefix}{line}").trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn table(rows: &[Value], issue_url: &str) -> String {
    let mut lines: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            children(row)
                .iter()
                .map(|cell| table_cell(cell, issue_url))
                .collect()
        })
        .collect();
    let has_header = rows.first().is_some_and(|row| {
        children(row)
            .iter()
            .all(|cell| node_type(cell) == "tableHeader")
    });
    if !has_header {
        // GFM has no table without a header row
        lines.insert(0, Vec::new());
    }
    let columns = lines.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return String::new();
    }
    let mut out: Vec<String> = lines
        .iter()
        .map(|cells| {
            let mut line = String::from("|");
            for column in 0..columns {
                let cell = cells.get(column).map_or("", String::as_str);
                line.push_str(&format!(" {cell} |"));
            }
            line
        })
        .collect();
    out.insert(1, format!("|{}", " --- |".repeat(columns)));
    out.join("\n")
}

// A GFM cell is one line of inline content: the blocks of an ADF cell are joined with spaces
fn table_cell(cell: &Value, issue_url: &str) -> String {
    blocks(children(cell), issue_url)
        .lines()
        .map(|line| {
            let backslashes = line.len() - line.trim_end_matches('\\').len();
            // An odd count ends in the backslash of a hard break, not in an escaped one
            &line[..line.len() - backslashes % 2]
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn media(node: &Value, issue_url: &str) -> String {
    let label = match node["attrs"]["alt"].as_str() {
        Some(alt) if !alt.is_empty() => alt,
        _ => "Attachment",
    };
    format!("[{}]({})", escape(label), link_target(issue_url))
}

fn inline(nodes: &[Value], issue_url: &str) -> String {
    let mut out: String = nodes
        .iter()
        .map(|node| match node_type(node) {
            "text" => text(node),
            "hardBreak" => "\\\n".to_owned(),
            "mention" => format!("@{}", escape(leaf_text(node).trim_start_matches('@'))),
            "mediaInline" => media(node, issue_url),
            _ if node.get("content").is_some() => inline(children(node), issue_url),
            _ => escape(leaf_text(node)),
        })
        .collect();
    // At the end of a block the backslash of a hard break would show up as text
    while out.ends_with("\\\n") {
        out.truncate(out.len() - 2);
    }
    out
}

fn text(node: &Value) -> String {
    let raw = leaf_text(node);
    let marked = raw.trim();
    if marked.is_empty() {
        return raw.to_owned();
    }
    let marks = node["marks"].as_array().map_or(&[][..], Vec::as_slice);
    let has = |name: &str| marks.iter().any(|mark| node_type(mark) == name);

    let mut out = if has("code") {
        code_span(marked)
    } else {
        escape(marked)
    };
    if has("strike") {
        out = format!("~~{out}~~");
    }
    if has("em") {
        out = format!("*{out}*");
    }
    if has("strong") {
        out = format!("**{out}**");
    }
    let link = marks.iter().find(|mark| node_type(mark) == "link");
    if let Some(href) = link.and_then(|mark| mark["attrs"]["href"].as_str()) {
        out = format!("[{out}]({})", link_target(href));
    }
    // `**bold **` is not emphasis in Markdown: the spaces go outside the markers
    let leading = &raw[..raw.len() - raw.trim_start().len()];
    let trailing = &raw[raw.trim_end().len()..];
    format!("{leading}{out}{trailing}")
}

fn code_span(code: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(code) + 1);
    if code.starts_with('`') || code.ends_with('`') {
        format!("{fence} {code} {fence}")
    } else {
        format!("{fence}{code}{fence}")
    }
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '#' | '&'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn link_target(url: &str) -> String {
    url.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ISSUE_URL: &str = "https://example.atlassian.net/browse/PROJ-1";

    fn doc(content: Value) -> Value {
        json!({ "type": "doc", "version": 1, "content": content })
    }

    fn text(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }

    fn marked(text: &str, marks: Value) -> Value {
        json!({ "type": "text", "text": text, "marks": marks })
    }

    fn paragraph(content: Value) -> Value {
        json!({ "type": "paragraph", "content": content })
    }

    fn item(content: Value) -> Value {
        json!({ "type": "listItem", "content": content })
    }

    fn markdown(content: Value) -> String {
        to_markdown(Some(&doc(content)), ISSUE_URL)
    }

    #[test]
    fn a_missing_description_converts_to_empty_output() {
        assert_eq!(to_plain_text(None), "");
        assert_eq!(to_markdown(None, ISSUE_URL), "");
        assert_eq!(to_plain_text(Some(&Value::Null)), "");
        assert_eq!(to_markdown(Some(&Value::Null), ISSUE_URL), "");
        assert_eq!(to_markdown(Some(&doc(json!([]))), ISSUE_URL), "");
    }

    #[test]
    fn plain_text_contains_the_text_of_every_text_node() {
        let doc = doc(json!([
            { "type": "heading", "attrs": { "level": 2 }, "content": [text("Steps")] },
            paragraph(json!([
                text("Open "),
                marked("the page", json!([{ "type": "strong" }])),
                { "type": "hardBreak" },
                text("and ask "),
                { "type": "mention", "attrs": { "id": "42", "text": "@Ann Lee" } },
            ])),
            { "type": "bulletList", "content": [
                item(json!([
                    paragraph(json!([text("first")])),
                    { "type": "orderedList", "content": [item(json!([paragraph(json!([text("nested")]))]))] },
                ])),
            ] },
            { "type": "codeBlock", "content": [text("let x = 1;")] },
            { "type": "panel", "attrs": { "panelType": "info" }, "content": [paragraph(json!([text("note")]))] },
            { "type": "table", "content": [
                { "type": "tableRow", "content": [
                    { "type": "tableHeader", "content": [paragraph(json!([text("head")]))] },
                    { "type": "tableCell", "content": [paragraph(json!([text("cell")]))] },
                ] },
            ] },
            { "type": "futureNode", "content": [paragraph(json!([text("unknown")]))] },
        ]));

        assert_eq!(
            to_plain_text(Some(&doc)),
            "Steps\nOpen the page\nand ask @Ann Lee\nfirst\nnested\nlet x = 1;\nnote\nhead\ncell\nunknown"
        );
    }

    #[test]
    fn paragraphs_are_separated_by_a_blank_line() {
        let content = json!([
            paragraph(json!([text("one")])),
            paragraph(json!([])),
            paragraph(json!([text("two"), { "type": "hardBreak" }, text("three")])),
        ]);
        assert_eq!(markdown(content), "one\n\ntwo\\\nthree");
    }

    #[test]
    fn headings_use_their_level() {
        let content = json!([
            { "type": "heading", "attrs": { "level": 1 }, "content": [text("Title")] },
            { "type": "heading", "attrs": { "level": 3 }, "content": [text("Part")] },
        ]);
        assert_eq!(markdown(content), "# Title\n\n### Part");
    }

    #[test]
    fn lists_nest_under_the_text_of_their_parent_item() {
        let content = json!([
            { "type": "orderedList", "attrs": { "order": 9 }, "content": [
                item(json!([
                    paragraph(json!([text("nine")])),
                    { "type": "bulletList", "content": [
                        item(json!([
                            paragraph(json!([text("inner")])),
                            { "type": "bulletList", "content": [item(json!([paragraph(json!([text("deep")]))]))] },
                        ])),
                        item(json!([paragraph(json!([text("second")]))])),
                    ] },
                ])),
                item(json!([
                    paragraph(json!([text("ten")])),
                    { "type": "codeBlock", "content": [text("code")] },
                ])),
            ] },
        ]);
        assert_eq!(
            markdown(content),
            "9. nine\n   - inner\n     - deep\n   - second\n10. ten\n\n    ```\n    code\n    ```"
        );
    }

    #[test]
    fn inline_marks_and_links_become_markdown() {
        let content = json!([paragraph(json!([
            marked("bold ", json!([{ "type": "strong" }])),
            marked("italic", json!([{ "type": "em" }])),
            text(" "),
            marked("gone", json!([{ "type": "strike" }])),
            text(" "),
            marked("both", json!([{ "type": "strong" }, { "type": "em" }])),
            text(" "),
            marked("a`b", json!([{ "type": "code" }])),
            text(" "),
            marked(
                "the docs",
                json!([{ "type": "link", "attrs": { "href": "https://example.com/a (b)" } }])
            ),
            text(" "),
            marked("plain", json!([{ "type": "underline" }])),
        ]))]);
        assert_eq!(
            markdown(content),
            "**bold** *italic* ~~gone~~ ***both*** ``a`b`` [the docs](https://example.com/a%20%28b%29) plain"
        );
    }

    #[test]
    fn markdown_syntax_in_text_is_escaped() {
        let content = json!([paragraph(json!([text("a_b *c* [d] <e> #1 `f` \\")]))]);
        assert_eq!(
            markdown(content),
            "a\\_b \\*c\\* \\[d\\] \\<e\\> \\#1 \\`f\\` \\\\"
        );
    }

    #[test]
    fn a_code_block_keeps_its_text_and_language() {
        let content = json!([
            { "type": "codeBlock", "attrs": { "language": "rust" }, "content": [text("fn main() {\n    a_b();\n}")] },
            { "type": "codeBlock", "content": [text("```\nfenced\n```")] },
        ]);
        assert_eq!(
            markdown(content),
            "```rust\nfn main() {\n    a_b();\n}\n```\n\n````\n```\nfenced\n```\n````"
        );
    }

    #[test]
    fn a_table_becomes_a_gfm_table() {
        let content = json!([{ "type": "table", "content": [
            { "type": "tableRow", "content": [
                { "type": "tableHeader", "content": [paragraph(json!([text("Name")]))] },
                { "type": "tableHeader", "content": [paragraph(json!([text("Value")]))] },
            ] },
            { "type": "tableRow", "content": [
                { "type": "tableCell", "content": [
                    paragraph(json!([text("a|b"), { "type": "hardBreak" }, text("c")])),
                    paragraph(json!([marked("x|y", json!([{ "type": "code" }]))])),
                ] },
                { "type": "tableCell", "content": [] },
            ] },
            { "type": "tableRow", "content": [
                { "type": "tableCell", "attrs": { "colspan": 2 }, "content": [paragraph(json!([text("merged")]))] },
            ] },
        ] }]);
        assert_eq!(
            markdown(content),
            "| Name | Value |\n| --- | --- |\n| a\\|b c `x\\|y` |  |\n| merged |  |"
        );
    }

    #[test]
    fn a_table_without_header_cells_gets_an_empty_header_row() {
        let content = json!([{ "type": "table", "content": [
            { "type": "tableRow", "content": [
                { "type": "tableCell", "content": [paragraph(json!([text("a")]))] },
                { "type": "tableCell", "content": [paragraph(json!([text("b")]))] },
            ] },
        ] }]);
        assert_eq!(markdown(content), "|  |  |\n| --- | --- |\n| a | b |");
    }

    #[test]
    fn a_panel_becomes_a_blockquote() {
        let content = json!([
            { "type": "panel", "attrs": { "panelType": "warning" }, "content": [
                paragraph(json!([text("careful")])),
                paragraph(json!([text("really")])),
            ] },
            { "type": "blockquote", "content": [paragraph(json!([text("quoted")]))] },
        ]);
        assert_eq!(markdown(content), "> careful\n>\n> really\n\n> quoted");
    }

    #[test]
    fn a_mention_becomes_the_name_with_an_at_sign() {
        let content = json!([paragraph(json!([
            text("cc "),
            { "type": "mention", "attrs": { "id": "42", "text": "@Ann Lee" } },
            text(" and "),
            { "type": "mention", "attrs": { "id": "43", "text": "Bob" } },
        ]))]);
        assert_eq!(markdown(content), "cc @Ann Lee and @Bob");
    }

    #[test]
    fn media_becomes_a_link_to_the_issue() {
        let content = json!([
            { "type": "mediaSingle", "attrs": { "layout": "center" }, "content": [
                { "type": "media", "attrs": { "type": "file", "id": "abc", "alt": "screen.png" } },
            ] },
            { "type": "mediaGroup", "content": [
                { "type": "media", "attrs": { "type": "file", "id": "def" } },
            ] },
            paragraph(json!([text("see "), { "type": "mediaInline", "attrs": { "id": "ghi" } }])),
        ]);
        assert_eq!(
            markdown(content),
            format!(
                "[screen.png]({ISSUE_URL})\n\n[Attachment]({ISSUE_URL})\n\nsee [Attachment]({ISSUE_URL})"
            )
        );
    }

    #[test]
    fn a_rule_becomes_a_thematic_break() {
        let content = json!([
            paragraph(json!([text("above")])),
            { "type": "rule" },
            paragraph(json!([text("below")])),
        ]);
        assert_eq!(markdown(content), "above\n\n---\n\nbelow");
    }

    #[test]
    fn an_unknown_node_keeps_its_text() {
        let content = json!([
            { "type": "expand", "attrs": { "title": "More" }, "content": [
                paragraph(json!([text("hidden "), { "type": "status", "attrs": { "text": "DONE", "color": "green" } }])),
            ] },
            { "type": "taskList", "content": [
                { "type": "taskItem", "attrs": { "state": "TODO" }, "content": [
                    text("call "),
                    marked("Ann", json!([{ "type": "strong" }])),
                ] },
            ] },
            paragraph(json!([
                { "type": "futureInline", "content": [text("wrapped")] },
                { "type": "futureLeaf", "attrs": { "id": 1 } },
            ])),
            { "no": "type" },
        ]);
        assert_eq!(markdown(content), "hidden DONE\n\ncall **Ann**\n\nwrapped");
    }
}
