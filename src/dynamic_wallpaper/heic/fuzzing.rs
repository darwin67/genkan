//! Feature-gated in-memory entrypoints. No native decode, file I/O or services.
use super::*;

pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Runs the real structural walker with production limits and also with a
/// tighter budget. Acceptance must be monotone as resources are increased.
pub fn container(bytes: &[u8]) {
    if bytes.len() > MAX_INPUT_BYTES {
        return;
    }
    let limits = Limits::default();
    let budget = container::Budget {
        max_container_items: limits.max_container_items,
        max_container_associations: limits.max_container_associations,
        max_metadata_items: limits.max_metadata_items as u32,
        max_metadata_bytes: limits.max_metadata_bytes as u64,
        max_container_boxes: limits.max_container_boxes,
        max_container_depth: limits.max_container_depth,
        max_item_extents: limits.max_item_extents,
    };
    let normal = container::preflight(bytes, &budget);
    let tight = container::Budget {
        max_container_items: 16,
        max_container_associations: 32,
        max_metadata_items: 4,
        max_metadata_bytes: 1024,
        max_container_boxes: 32,
        max_container_depth: 4,
        max_item_extents: 4,
    };
    if container::preflight(bytes, &tight).is_ok() {
        assert!(normal.is_ok());
    }
}

/// Exercises XMP, raw XML/binary plist, and whitespace-tolerant Base64 paths.
/// The same parser budgets used by Document are retained. The extra input cap
/// bounds the fuzzer itself; RSS limits are containment, not an allocator proof.
pub fn metadata(bytes: &[u8]) {
    if bytes.len() > MAX_INPUT_BYTES {
        return;
    }
    let limits = Limits::default();
    let images = TopLevelImages::new((1..=64).map(HeifItemId::new).collect());
    if let Ok(properties) = parse_xmp(bytes, &limits) {
        for (name, plist) in properties {
            assert!(plist.len() <= limits.max_plist_bytes);
            let _ = parse_property(name, &plist, &images, &limits);
        }
    }
    for name in [PropertyName::H24, PropertyName::Solar, PropertyName::Apr] {
        let _ = parse_property(name, bytes, &images, &limits);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        if let Ok(plist) = decode_plist_base64(text, &limits) {
            assert!(plist.len() <= limits.max_plist_bytes);
            for name in [PropertyName::H24, PropertyName::Solar, PropertyName::Apr] {
                let _ = parse_property(name, &plist, &images, &limits);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_heavy_xmp_does_not_invent_a_property_count_limit() {
        let xml = format!(
            "<r xmlns:a=\"http://ns.apple.com/namespace/1.0/\">{}</r>",
            "<x a:h24=\"\" a:solar=\"\" a:apr=\"\"/>".repeat(1366)
        );
        assert!(xml.len() < MAX_INPUT_BYTES);
        let properties = parse_xmp(xml.as_bytes(), &Limits::default()).unwrap();
        assert_eq!(properties.len(), 4098);
        // Empty plists are rejected normally downstream, not by a panic in
        // the fuzz harness. Attribute count is not XML event count.
        metadata(xml.as_bytes());
    }

    #[test]
    fn production_budgets_reject_one_over_the_limit() {
        // Count valid, empty boxes: this reaches the aggregate box budget,
        // rather than just rejecting a truncated/invalid input earlier.
        let mut bytes = Vec::from(&b"\0\0\0\x0cmeta\0\0\0\0"[..]);
        for _ in 0..32 {
            bytes.extend_from_slice(b"\0\0\0\x08free");
        }
        let budget = container::Budget {
            max_container_items: 16,
            max_container_associations: 32,
            max_metadata_items: 4,
            max_metadata_bytes: 1024,
            max_container_boxes: 33,
            max_container_depth: 4,
            max_item_extents: 4,
        };
        assert!(container::preflight(&bytes, &budget).is_ok());
        assert!(matches!(
            container::preflight(
                &bytes,
                &container::Budget {
                    max_container_boxes: 32,
                    ..budget
                }
            ),
            Err(Error::Limit("box count"))
        ));
        let limits = Limits {
            max_plist_bytes: 16,
            ..Limits::default()
        };
        assert!(decode_plist_base64("QUFBQUFBQUFBQUFBQUFBQQ==", &limits).is_ok());
        assert!(matches!(
            decode_plist_base64("QUFBQUFBQUFBQUFBQUFBQUE=", &limits),
            Err(Error::Limit("property list size"))
        ));
        let xml = b"<plist><dict/></plist>";
        assert!(preflight_xml_plist(
            xml,
            &Limits {
                max_plist_bytes: xml.len(),
                ..Limits::default()
            }
        )
        .is_ok());
        assert!(matches!(
            preflight_xml_plist(
                xml,
                &Limits {
                    max_plist_bytes: xml.len() - 1,
                    ..Limits::default()
                }
            ),
            Err(Error::Limit("property list size"))
        ));
    }
}
