use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn buffer_arithmetic_is_checked(
        width in prop_oneof![0u32..=16_385, any::<u32>()],
        height in prop_oneof![0u32..=16_385, any::<u32>()],
        scale in prop_oneof![0i32..=4, any::<i32>()],
    ) {
        let w = u128::from(width) * scale.max(1) as u128;
        let h = u128::from(height) * scale.max(1) as u128;
        let valid = w <= u128::from(MAX_SURFACE_DIMENSION) && h <= u128::from(MAX_SURFACE_DIMENSION) && w * h * 4 <= MAX_BUFFER_BYTES as u128;
        let result = buffer_geometry((width, height), scale);
        prop_assert_eq!(result.is_ok(), valid);
        if let Ok(geometry) = result {
            prop_assert_eq!(u128::from(geometry.width), w);
            prop_assert_eq!(u128::from(geometry.height), h);
            prop_assert_eq!(geometry.bytes as u128, w * h * 4);
        }
    }

    #[test]
    fn cover_sampler_is_centered_and_covers(sw in 1u32..32, sh in 1u32..32, width in 1u32..32, height in 1u32..32) {
        // Coordinate-coded source makes crop, channel order, coverage and
        // nearest-neighbor sampling observable without a second renderer.
        let pixels = (0..sh).flat_map(|y| (0..sw).flat_map(move |x| [x as u8, y as u8, 73, 255])).collect();
        let frame = RgbaFrame { width: sw, height: sh, pixels };
        let mut target = vec![0; (width * height * 4) as usize];
        render_cover_argb(&mut target, width, height, Some(&frame));
        for pixel in target.chunks_exact(4) {
            prop_assert_eq!(pixel[0], 73);
            prop_assert_eq!(pixel[3], 255);
            prop_assert!(u32::from(pixel[2]) < sw && u32::from(pixel[1]) < sh);
        }
        let first = &target[..4];
        let last = &target[target.len()-4..];
        // In source units, the crop and sample floors each cost one scaled
        // pixel. Flooring a positive scaled dimension can halve the ideal
        // scale, so four ideal pixel quanta plus one source pixel bound the
        // two endpoint errors even for a one-pixel destination.
        let quantum = (f64::from(sw) / f64::from(width)).min(f64::from(sh) / f64::from(height)).ceil() as i32;
        prop_assert!((i32::from(first[2]) + i32::from(last[2]) - (sw as i32 - 1)).abs() <= 4 * quantum + 1);
        prop_assert!((i32::from(first[1]) + i32::from(last[1]) - (sh as i32 - 1)).abs() <= 4 * quantum + 1);
        for row in target.chunks_exact((width * 4) as usize) {
            prop_assert!(row.chunks_exact(4).map(|p| p[2]).collect::<Vec<_>>().windows(2).all(|p| p[0] <= p[1]));
        }
    }
}
