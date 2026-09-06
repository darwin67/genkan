const INTERNAL_FALLBACK: &str = "eDP-1";

/// Selects the connector that presents authentication.
///
/// An explicitly requested connector wins when present. Otherwise a named
/// external output is preferred deterministically, with `eDP-1` as the
/// internal-panel fallback. Unnamed outputs remain usable as a last resort.
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
            .find(|(_, name)| name.is_some_and(|name| name == requested))
        {
            return Some(output.clone());
        }
    }

    outputs
        .iter()
        .filter_map(|(output, name)| {
            name.filter(|name| *name != INTERNAL_FALLBACK)
                .map(|name| (name, output))
        })
        .min_by_key(|(name, _)| *name)
        .map(|(_, output)| output.clone())
        .or_else(|| {
            outputs
                .iter()
                .find(|(_, name)| name == &Some(INTERNAL_FALLBACK))
                .map(|(output, _)| output.clone())
        })
        .or_else(|| outputs.first().map(|(output, _)| output.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_available_output_wins() {
        let outputs = [(1, Some("eDP-1")), (2, Some("DP-2")), (3, Some("DP-1"))];
        assert_eq!(select(outputs, Some("DP-2")), Some(2));
    }

    #[test]
    fn external_output_is_the_deterministic_default() {
        let outputs = [(1, Some("eDP-1")), (2, Some("DP-2")), (3, Some("DP-1"))];
        assert_eq!(select(outputs, None), Some(3));
    }

    #[test]
    fn internal_panel_is_the_fallback() {
        let outputs = [(1, Some("eDP-1"))];
        assert_eq!(select(outputs, None), Some(1));
        assert_eq!(select(outputs, Some("DP-1")), Some(1));
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
