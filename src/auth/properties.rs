use super::*;
use proptest::prelude::*;

fn argument() -> impl Strategy<Value = String> {
    prop_oneof![
        proptest::collection::vec(any::<char>().prop_filter("no NUL", |c| *c != '\0'), 0..96)
            .prop_map(|chars| chars.into_iter().collect()),
        prop::sample::select(vec![
            "",
            "'",
            "\"",
            "$HOME",
            "`exit 93`",
            "$(exit 94)",
            "*?[abc]",
            "a\nb",
            "空 白",
            "'; exit 95; #",
        ])
        .prop_map(str::to_owned),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn shell_round_trip(arguments in prop::collection::vec(argument(), 0..16)) {
        let Request::StartSession { cmd, env } = start_session_request(&arguments, vec![]) else {
            unreachable!();
        };
        prop_assert!(env.is_empty());
        // POSIX printf prints one empty field with zero operands. Check the
        // empty encoding separately, without inventing a sentinel argument.
        let script = if arguments.is_empty() {
            prop_assert!(cmd.is_empty());
            "exit 0".to_owned()
        } else {
            format!("exec printf '%s\\0' {}", cmd.join(" "))
        };
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &script]).output().unwrap();
        prop_assert!(output.status.success());
        prop_assert!(output.stderr.is_empty());
        let expected: Vec<u8> = arguments.iter()
            .flat_map(|argument| argument.bytes().chain([0])).collect();
        prop_assert_eq!(output.stdout, expected);
    }
}
