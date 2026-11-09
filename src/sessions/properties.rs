use super::*;
use proptest::prelude::*;

// Two escaping layers: Desktop Entry string escaping, then Exec quoting.
// Exec itself intentionally accepts ASCII only; % is doubled as a field-code
// literal, not banned from the resulting argv.
fn quote(argument: &str) -> String {
    let mut exec = String::from("\"");
    for c in argument.chars() {
        match c {
            '%' => exec.push_str("%%"),
            '"' | '`' | '$' | '\\' => {
                exec.push('\\');
                exec.push(c);
            }
            _ => exec.push(c),
        }
    }
    exec.push('"');
    exec.replace('\\', "\\\\")
}

fn source(raw: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName=Synthetic\nExec={raw}\n")
}

fn expand(raw: &str) -> Option<Vec<String>> {
    let source = source(raw);
    let file = parse(source.as_bytes()).ok()?;
    expand_exec(&file, Path::new("/synthetic/session.desktop"), "Synthetic")
}

/// The Exec value as the Desktop Entry parser reads it: trimmed of the
/// whitespace around `=` and the line end, which may be any Unicode
/// whitespace, not only spaces.
fn parsed_exec(raw: &str) -> Option<String> {
    let source = source(raw);
    let file = parse(source.as_bytes()).ok()?;
    Some(file.exec()?.raw().to_owned())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn desktop_quoting_round_trip(args in prop::collection::vec("[ -~]{0,48}", 0..12)) {
        let raw = format!("program {}", args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" "));
        prop_assert!(strict_exec(&raw).is_some());
        let actual = expand(&raw).unwrap();
        prop_assert_eq!(&actual[0], "program");
        prop_assert_eq!(&actual[1..], args);
    }

    #[test]
    fn arbitrary_exec_never_panics(raw in prop::collection::vec(any::<char>(), 0..1024).prop_map(|chars| chars.into_iter().collect::<String>())) {
        if let Some(command) = expand(&raw) {
            // The strict check sees the parsed value, so `Exec=<VT>+` is the
            // command "+", not a rejected control character.
            let value = parsed_exec(&raw).unwrap();
            prop_assert!(raw.contains(&value));
            prop_assert!(strict_exec(&value).is_some());
            prop_assert!(!command.is_empty());
            prop_assert!(!command[0].contains('='));
        }
        let _ = strict_desktop_file(raw.as_bytes());
    }

    #[test]
    fn unquoted_reserved_are_rejected(c in prop::sample::select(
        "'\\><~|&;$*?#()`".chars().collect::<Vec<_>>()
    ), suffix in "[a-z0-9]{0,24}") {
        // A raw \r is a valid Desktop Entry escape for a separator, not
        // an unquoted backslash. Encode the literal at the string layer.
        let literal = if c == '\\' { "\\\\".to_owned() } else { c.to_string() };
        let raw = format!("program a{literal}{suffix}");
        prop_assert!(is_unquoted_reserved(c));
        prop_assert!(strict_exec(&raw).is_none());
        prop_assert!(expand(&raw).is_none());
    }

    #[test]
    fn field_codes_expand_once(name in "[ -~]{1,32}") {
        let raw = "program %c %k %% %d %D %n %N %v %m";
        let source = format!("[Desktop Entry]\nType=Application\nName=Synthetic\nExec={raw}\n");
        let file = parse(source.as_bytes()).unwrap();
        let result = expand_exec(&file, Path::new("/synthetic/session.desktop"), &name).unwrap();
        prop_assert_eq!(result, vec!["program".to_owned(), name, "/synthetic/session.desktop".to_owned(), "%".to_owned()]);
    }

    #[test]
    fn unsupported_codes_never_survive(code in prop::sample::select(vec!['f','F','u','U','i','x','z'])) {
        let raw = format!("program %{code}");
        prop_assert!(expand(&raw).is_none());
    }
}
