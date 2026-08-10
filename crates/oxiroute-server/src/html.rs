pub(crate) fn escape_html(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            character => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escapes_html_metacharacters() {
        let mut output = String::new();
        escape_html(&mut output, "<&>\"'");
        assert_eq!(output, "&lt;&amp;&gt;&quot;&#39;");
    }
}
