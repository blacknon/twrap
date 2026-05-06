pub(crate) fn expand_template<F>(template: &str, mut resolve: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut rendered = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            rendered.push(ch);
            continue;
        }

        let mut token = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '}' {
                closed = true;
                break;
            }
            token.push(next);
        }

        if !closed {
            rendered.push('{');
            rendered.push_str(&token);
            break;
        }

        if let Some(value) = resolve(&token) {
            rendered.push_str(&value);
        } else {
            rendered.push('{');
            rendered.push_str(&token);
            rendered.push('}');
        }
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_template_preserves_unknown_tokens() {
        let rendered = expand_template("a-{known}-{unknown}", |token| match token {
            "known" => Some("ok".to_string()),
            _ => None,
        });

        assert_eq!(rendered, "a-ok-{unknown}");
    }

    #[test]
    fn expand_template_preserves_unclosed_token() {
        let rendered = expand_template("a-{broken", |_| Some("x".to_string()));
        assert_eq!(rendered, "a-{broken");
    }
}
