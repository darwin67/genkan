use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn recency_policy(times in prop_oneof![
        prop::collection::vec(prop::option::of(-2i64..100), 0..32),
        prop::collection::vec(1i64..100, 0..32).prop_map(|times| times.into_iter().map(Some).collect()),
    ], rotation in any::<usize>()) {
        // Use the real ingestion path: zero and negative timestamps become
        // unknown, and an unknown peer prevents automatic selection.
        let mut accounts: Vec<_> = times.iter().enumerate().map(|(id, time)| {
            Account::from_properties(Properties {
                username: format!("synthetic-{id}"), real_name: String::new(),
                system_account: false, locked: false, last_login: *time,
            }).unwrap()
        }).collect();
        let expected = if accounts.len() == 1 { Some(0) }
            else if times.iter().any(|time| time.is_none_or(|time| time <= 0)) { None }
            else {
                let greatest = times.iter().flatten().max();
                let winners: Vec<_> = times.iter().enumerate().filter(|(_, time)| time.as_ref() == greatest).map(|(id, _)| id).collect();
                (winners.len() == 1).then(|| winners[0])
            }.map(|id| format!("synthetic-{id}"));
        prop_assert_eq!(preferred_account(&accounts).map(|account| account.username.clone()), expected.clone());
        if !accounts.is_empty() { let count = accounts.len(); accounts.rotate_left(rotation % count); }
        accounts.reverse();
        prop_assert_eq!(preferred_account(&accounts).map(|account| account.username.clone()), expected);
    }

    #[test]
    fn unicode_identity_and_label_contract(value in prop::collection::vec(any::<char>(), 0..600).prop_map(|v| v.into_iter().collect::<String>())) {
        let valid = valid_username(&value);
        prop_assert_eq!(valid, !value.is_empty() && value.chars().count() <= 256 && value.chars().all(|c|
            !c.is_control() && !c.is_whitespace() && get_general_category(c) != GeneralCategory::Format));
        if let Some(label) = presentation_label(&value) {
            prop_assert!(!label.is_empty());
            prop_assert!(label.chars().count() <= MAX_LABEL_CHARS);
            prop_assert!(label.chars().all(|c| !c.is_control() && get_general_category(c) != GeneralCategory::Format));
        }
    }
}
