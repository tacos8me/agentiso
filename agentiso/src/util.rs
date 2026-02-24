/// Shell-escape a string by wrapping it in single quotes.
///
/// Single quotes inside the string are handled by ending the single-quoted
/// segment, inserting an escaped single quote, and starting a new segment:
/// `it's` becomes `'it'\''s'`.
pub(crate) fn shell_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len() + 2);
    escaped.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            escaped.push_str("'\\''");
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('\'');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_simple_path() {
        assert_eq!(shell_escape("/home/user/project"), "'/home/user/project'");
    }

    #[test]
    fn shell_escape_path_with_spaces() {
        assert_eq!(
            shell_escape("/home/user/my project"),
            "'/home/user/my project'"
        );
    }

    #[test]
    fn shell_escape_path_with_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_path_with_shell_metacharacters() {
        assert_eq!(
            shell_escape("/path/$(whoami)/`id`"),
            "'/path/$(whoami)/`id`'"
        );
    }
}
