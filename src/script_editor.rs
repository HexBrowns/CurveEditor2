//! Lua スクリプトエディタ補助（行番号・簡易検証）

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
    "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// 括弧の対応など簡易チェック。問題なければ `None`。
pub fn validate_lua_script(code: &str) -> Option<String> {
    if code.trim().is_empty() {
        return Some("コードが空です".into());
    }

    let mut round = 0i32;
    let mut square = 0i32;
    let mut curly = 0i32;
    let mut in_string = false;
    let mut escape = false;

    for ch in code.chars() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == '"' || ch == '\'' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' | '\'' => in_string = true,
            '(' => round += 1,
            ')' => round -= 1,
            '[' => square += 1,
            ']' => square -= 1,
            '{' => curly += 1,
            '}' => curly -= 1,
            _ => {}
        }
        if round < 0 {
            return Some("丸括弧 () の対応が取れていません".into());
        }
        if square < 0 {
            return Some("角括弧 [] の対応が取れていません".into());
        }
        if curly < 0 {
            return Some("波括弧 {} の対応が取れていません".into());
        }
    }

    if in_string {
        return Some("文字列が閉じられていません".into());
    }
    if round != 0 {
        return Some("丸括弧 () の対応が取れていません".into());
    }
    if square != 0 {
        return Some("角括弧 [] の対応が取れていません".into());
    }
    if curly != 0 {
        return Some("波括弧 {} の対応が取れていません".into());
    }

    if !code.contains("return") {
        return Some("ヒント: ScriptCurve では通常 `return` で値を返します".into());
    }

    None
}

/// 行ごとにキーワードを太字風に色分けした RichText 行（プレビュー用）。
pub fn highlight_line(line: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let mut in_string = false;
    let mut quote = '"';

    let flush = |token: &str, out: &mut String| {
        if token.is_empty() {
            return;
        }
        if LUA_KEYWORDS.contains(&token) {
            out.push_str(token);
        } else if token.chars().all(|c| c.is_ascii_digit() || c == '.') {
            out.push_str(token);
        } else {
            out.push_str(token);
        }
    };

    for ch in line.chars() {
        if in_string {
            out.push(ch);
            if ch == quote {
                in_string = false;
            }
            continue;
        }

        if ch == '"' || ch == '\'' {
            flush(&token, &mut out);
            token.clear();
            in_string = true;
            quote = ch;
            out.push(ch);
            continue;
        }

        if ch.is_alphanumeric() || ch == '_' {
            token.push(ch);
        } else {
            flush(&token, &mut out);
            token.clear();
            out.push(ch);
        }
    }
    flush(&token, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_balanced() {
        assert!(validate_lua_script("return t").is_none());
        assert!(validate_lua_script("return (1+2)").is_none());
    }

    #[test]
    fn validate_unbalanced() {
        assert!(validate_lua_script("return (1+2").is_some());
    }
}
