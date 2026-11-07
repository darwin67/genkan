use super::*;
use proptest::prelude::*;

fn connector() -> impl Strategy<Value = String> {
    prop_oneof![
        ".{0,64}",
        "(eDP-|LVDS-|DSI-|DP-|HDMI-A-)[0-9]{0,40}",
        prop::sample::select(vec!["DP-2", "DP-02", "DP-10", "eDP-", "LVDS-0", "DSI-1"])
            .prop_map(str::to_owned),
    ]
}

fn internal(name: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|p| name.starts_with(p))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn selection_policy(
        names in prop::collection::vec(prop::option::of(connector()), 1..32),
        index in any::<usize>(), keys in prop::collection::vec(any::<u64>(), 32),
    ) {
        let outputs: Vec<_> = names.iter().enumerate().map(|(id, name)| (id, name.as_deref())).collect();
        let selected = select(outputs.clone(), None).unwrap();
        prop_assert!(selected < names.len());
        if names.iter().flatten().any(|name| !internal(name)) {
            prop_assert!(names[selected].as_deref().is_some_and(|name| !internal(name)));
        }
        // Duplicate connector names are indistinguishable by policy, and all
        // unnamed outputs deliberately fall back to discovery order. Compare
        // connector identity, not an arbitrary compositor handle, in those cases.
        let reversed = select(outputs.iter().copied().rev(), None).unwrap();
        prop_assert_eq!(&names[selected], &names[reversed]);
        let mut shuffled = outputs.clone();
        shuffled.sort_by_key(|(id, _)| keys[*id]);
        let permuted = select(shuffled, None).unwrap();
        prop_assert_eq!(&names[selected], &names[permuted]);
        let requested = names[index % names.len()].as_deref();
        if let Some(requested) = requested {
            let explicit = select(outputs, Some(requested)).unwrap();
            prop_assert_eq!(names[explicit].as_deref(), Some(requested));
        }
    }

    #[test]
    fn connector_order_is_total(a in connector(), b in connector(), c in connector()) {
        let ab = connector_order(&a).cmp(&connector_order(&b));
        let ba = connector_order(&b).cmp(&connector_order(&a));
        prop_assert_eq!(ab, ba.reverse());
        prop_assert_eq!(ab.is_eq(), a == b);
        if ab.is_le() && connector_order(&b) <= connector_order(&c) {
            prop_assert!(connector_order(&a) <= connector_order(&c));
        }
        prop_assert_eq!(connector_order(&a).cmp(&connector_order(&a)), std::cmp::Ordering::Equal);
    }

    #[test]
    fn numeric_suffix_uses_numeric_not_lexical_order(a in 0u64..u64::MAX, b in 0u64..u64::MAX) {
        let left = format!("DP-{a}");
        let right = format!("DP-{b}");
        prop_assert_eq!(connector_order(&left).cmp(&connector_order(&right)), a.cmp(&b));
    }
}
