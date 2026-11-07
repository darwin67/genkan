#[cfg(feature = "wayland")]
pub mod names;

/// Selects the connector that presents authentication.
///
/// An explicitly requested connector wins when present. Otherwise a named
/// external output is preferred, falling back to eDP-, LVDS-, or DSI- panels.
/// Names sort by connector type and then numeric suffix, independent of
/// discovery order. Unnamed outputs remain usable as a last resort.
pub fn select<'a, T>(
    outputs: impl IntoIterator<Item = (T, Option<&'a str>)>,
    requested: Option<&str>,
) -> Option<T>
where
    T: Clone,
{
    let outputs = outputs.into_iter().collect::<Vec<_>>();
    if let Some(requested) = requested {
        if let Some((output, _)) = outputs
            .iter()
            .find(|(_, name)| name.is_some_and(|name| name.eq_ignore_ascii_case(requested)))
        {
            return Some(output.clone());
        }
    }

    outputs
        .iter()
        .filter_map(|(output, name)| name.map(|name| (name, output)))
        .min_by_key(|(name, _)| connector_order(name))
        .map(|(_, output)| output.clone())
        .or_else(|| outputs.first().map(|(output, _)| output.clone()))
}

fn connector_order(name: &str) -> (bool, &str, usize, &str, &str) {
    let internal = ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|prefix| name.starts_with(prefix));
    let connector = name.trim_end_matches(|character: char| character.is_ascii_digit());
    let number = name[connector.len()..].trim_start_matches('0');
    // Comparing digit counts before digits avoids integer overflow for names
    // supplied by the compositor. The full name breaks leading-zero ties.
    (internal, connector, number.len(), number, name)
}

#[cfg(test)]
mod properties;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_available_output_wins() {
        let outputs = [(1, Some("eDP-1")), (2, Some("DP-2")), (3, Some("DP-1"))];
        assert_eq!(select(outputs, Some("DP-2")), Some(2));
        assert_eq!(select(outputs, Some("eDP-1")), Some(1));
    }

    #[test]
    fn external_output_is_the_deterministic_default() {
        let outputs = [(1, Some("eDP-1")), (2, Some("DP-2")), (3, Some("DP-1"))];
        assert_eq!(select(outputs, None), Some(3));
    }

    #[test]
    fn internal_panel_is_the_fallback() {
        for panel in ["eDP-2", "LVDS-1", "DSI-3"] {
            let outputs = [(1, Some(panel)), (2, Some("HDMI-A-1"))];
            assert_eq!(select(outputs, None), Some(2), "{panel}");
            assert_eq!(select(outputs, Some(panel)), Some(1), "{panel}");
            assert_eq!(select([(1, Some(panel))], Some("absent")), Some(1));
        }
    }

    #[test]
    fn connector_type_precedes_natural_numeric_order() {
        let names = ["HDMI-A-1", "DP-10", "DP-2", "DSI-1", "LVDS-1", "eDP-2"];
        let outputs = names
            .into_iter()
            .enumerate()
            .map(|(id, name)| (id, Some(name)))
            .collect::<Vec<_>>();
        assert_eq!(select(outputs.clone(), None), Some(2));
        assert_eq!(select(outputs.into_iter().rev(), None), Some(2));
        assert_eq!(
            select(
                [(1, Some("DP-999999999999999999999")), (2, Some("DP-10"))],
                None
            ),
            Some(2)
        );
        assert_eq!(
            select([(1, Some("DP-02")), (2, Some("DP-2"))], None),
            Some(1)
        );
        assert_eq!(
            select([(1, Some("Virtual")), (2, Some("DP-2"))], None),
            Some(2)
        );
        assert_eq!(
            select([(1, Some("eDP-10")), (2, Some("eDP-2"))], None),
            Some(2)
        );
    }

    #[test]
    fn unnamed_output_is_still_usable() {
        assert_eq!(select([(7, None)], None), Some(7));
        assert_eq!(select::<u32>([], None), None);
    }

    #[test]
    fn discovery_order_does_not_change_named_selection() {
        let first = [(1, Some("DP-2")), (2, Some("eDP-1")), (3, Some("DP-1"))];
        let reversed = [(3, Some("DP-1")), (2, Some("eDP-1")), (1, Some("DP-2"))];

        assert_eq!(select(first, None), Some(3));
        assert_eq!(select(reversed, None), Some(3));
    }

    #[test]
    fn named_fallback_precedes_unnamed_outputs() {
        let outputs = [(1, None), (2, Some("eDP-1"))];

        assert_eq!(select(outputs, None), Some(2));
        assert_eq!(select(outputs, Some("DP-1")), Some(2));
    }
}
