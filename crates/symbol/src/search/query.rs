//! The content query language, compiled to an FTS5 `MATCH` expression.
//!
//! | Input               | Meaning        | Emitted                     |
//! |---------------------|----------------|-----------------------------|
//! | `kan extension`     | every word     | `("kan" "extension")`       |
//! | `"riemann surface"` | a phrase       | `("riemann surface")`       |
//! | `topos OR locale`   | either         | `("topos" OR "locale")`     |
//! | `ext*`              | a prefix       | `("ext"*)`                  |
//! | `-sheaf`            | excluded       | `(…) NOT "sheaf"`           |
//!
//! Every user token is emitted as a quoted FTS5 string with inner quotes
//! doubled, so the expression is always well formed and the user cannot reach
//! column filters, `NEAR`, or the unquoted operators. Porter stemming and
//! diacritic folding are the index's, so `extensions` finds `extension` and
//! `cech` finds `Čech`. A hyphenated word such as `0-1-category` becomes a
//! phrase, because the tokenizer splits it the same way on both sides.

/// One term of the query, before it becomes FTS5 syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A word, a `"phrase"`, or a `prefix*`; the flag marks the prefix.
    Term {
        text: String,
        prefix: bool,
    },
    Exclude(String),
    Or,
}

/// Splits `query` into words, keeping `"quoted phrases"` together. A lone
/// `OR` between words is the operator; `-word` excludes; a trailing `*`
/// outside quotes asks for a prefix.
fn tokenize(query: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(&character) = chars.peek() {
        if character.is_whitespace() {
            chars.next();
            continue;
        }
        let exclude = character == '-' && {
            chars.next();
            true
        };
        let mut text = String::new();
        let mut prefix = false;
        if chars.peek() == Some(&'"') {
            chars.next();
            for character in chars.by_ref() {
                if character == '"' {
                    break;
                }
                text.push(character);
            }
        } else {
            while let Some(&character) = chars.peek() {
                if character.is_whitespace() {
                    break;
                }
                chars.next();
                text.push(character);
            }
            if text.ends_with('*') {
                text.truncate(text.trim_end_matches('*').len());
                prefix = true;
            }
        }
        if !exclude && !prefix && text == "OR" {
            tokens.push(Token::Or);
            continue;
        }
        // Only letters, digits and the characters FTS5 keeps inside a string
        // carry meaning; a token of nothing else, like a stray quote, is
        // dropped rather than searched for.
        let text = text
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_string();
        if text.is_empty() {
            continue;
        }
        tokens.push(if exclude {
            Token::Exclude(text)
        } else {
            Token::Term { text, prefix }
        });
    }
    tokens
}

/// `"text"` with inner quotes doubled, as an FTS5 string.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// The FTS5 `MATCH` expression for `query`, or why there is none: a query
/// without a positive term, such as one made only of exclusions, has nothing
/// to search for.
pub fn to_match(query: &str) -> Result<String, &'static str> {
    let tokens = tokenize(query);
    let mut positive: Vec<String> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut pending_or = false;
    for token in tokens {
        match token {
            Token::Term { text, prefix } => {
                let mut term = quoted(&text);
                if prefix {
                    term.push('*');
                }
                // `OR` only joins two terms; at the start or end it is noise.
                if pending_or && !positive.is_empty() {
                    positive.push("OR".to_string());
                }
                pending_or = false;
                positive.push(term);
            }
            Token::Exclude(text) => excluded.push(quoted(&text)),
            Token::Or => pending_or = true,
        }
    }
    if positive.is_empty() {
        return Err(if excluded.is_empty() {
            "error: q has no words to search for\n"
        } else {
            "error: q needs a word to search for besides the excluded ones\n"
        });
    }
    let mut expression = format!("({})", positive.join(" "));
    for term in excluded {
        expression.push_str(" NOT ");
        expression.push_str(&term);
    }
    Ok(expression)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_phrases_prefixes_and_exclusions_compile_to_fts5() {
        for (query, expected) in [
            ("kan extension", r#"("kan" "extension")"#),
            ("  Kan   extension ", r#"("Kan" "extension")"#),
            (r#""riemann surface""#, r#"("riemann surface")"#),
            (
                r#""riemann surface" sheaf"#,
                r#"("riemann surface" "sheaf")"#,
            ),
            ("topos OR locale", r#"("topos" OR "locale")"#),
            (
                "topos OR locale -sheaf",
                r#"("topos" OR "locale") NOT "sheaf""#,
            ),
            ("ext*", r#"("ext"*)"#),
            ("kan ext**", r#"("kan" "ext"*)"#),
            ("0-1-category", r#"("0-1-category")"#),
            ("kan -lift -sheaf", r#"("kan") NOT "lift" NOT "sheaf""#),
            // Operators that FTS5 would otherwise interpret are just strings.
            ("a NEAR b", r#"("a" "NEAR" "b")"#),
            ("title:kan", r#"("title:kan")"#),
            ("(kan)", r#"("kan")"#),
            (r#"say "hi"#, r#"("say" "hi")"#),
            ("OR kan OR", r#"("kan")"#),
            ("kan OR OR ext", r#"("kan" OR "ext")"#),
            ("Čech", r#"("Čech")"#),
        ] {
            assert_eq!(to_match(query).as_deref(), Ok(expected), "{query}");
        }
    }

    #[test]
    fn inner_quotes_are_doubled() {
        let query = "\"a\"\"b\"";
        assert_eq!(to_match(query).as_deref(), Ok(r#"("a" "b")"#));
        assert_eq!(
            quoted("it\"s"),
            "\"it\"\"s\"",
            "a quote inside a token is an escaped quote, not the end of it"
        );
    }

    #[test]
    fn a_query_needs_a_positive_term() {
        assert!(to_match("").is_err());
        assert!(to_match("   ").is_err());
        assert!(to_match("-sheaf").is_err());
        assert!(to_match("OR").is_err());
        assert!(to_match("\"\" * - ?").is_err());
    }
}
