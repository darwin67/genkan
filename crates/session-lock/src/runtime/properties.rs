use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn buffer_size_never_wraps(
        width in prop_oneof![0u32..=16_385, any::<u32>()],
        height in prop_oneof![0u32..=16_385, any::<u32>()],
        scale in prop_oneof![0u32..=4, any::<u32>()],
    ) {
        let w = u64::from(width) * u64::from(scale);
        let h = u64::from(height) * u64::from(scale);
        let valid = w > 0 && h > 0 && w <= u64::from(MAX_SURFACE_DIMENSION) && h <= u64::from(MAX_SURFACE_DIMENSION);
        let result = buffer_size(width, height, scale, wl_output::Transform::Normal);
        prop_assert_eq!(result.is_ok(), valid);
        if let Ok((actual_w, actual_h)) = result { prop_assert_eq!((u64::from(actual_w), u64::from(actual_h)), (w,h)); }
    }

    #[test]
    fn cover_geometry_preserves_ratio_and_centers(sw in 1u32..=16_384, sh in 1u32..=16_384, w in 1u32..=16_384, h in 1u32..=16_384) {
        let (x,y,width,height) = cover_geometry(sw,sh,w,h).unwrap();
        prop_assert!(width >= u128::from(w) && height >= u128::from(h));
        prop_assert!(width == u128::from(w) || height == u128::from(h));
        // Flooring the scaled axis introduces less than one pixel of error.
        let error = (width * u128::from(sh)).abs_diff(height * u128::from(sw));
        prop_assert!(error < u128::from(sw.max(sh)));
        prop_assert!((width - u128::from(w)).abs_diff(x * 2) <= 1);
        prop_assert!((height - u128::from(h)).abs_diff(y * 2) <= 1);
        prop_assert!(source_coordinate(w-1,x,width,sw) < sw);
        prop_assert!(source_coordinate(h-1,y,height,sh) < sh);
    }

    #[test]
    fn cover_sampling_uses_valid_centered_pixels(sw in 1u32..32, sh in 1u32..32, w in 1u32..32, h in 1u32..32) {
        let pixels: Vec<u8> = (0..sh).flat_map(|y| (0..sw).flat_map(move |x| [x as u8,y as u8,73,255])).collect();
        let frame = RgbaFrame::new(sw,sh,pixels.into()).unwrap();
        let first = sample_cover(&frame,w,h,0,0).unwrap();
        let last = sample_cover(&frame,w,h,w-1,h-1).unwrap();
        prop_assert_eq!(first[2],73);
        prop_assert_eq!(last[2],73);
        // Two floors and a floored scaled dimension: see the desktop sampler
        // property for the conservative source-space quantization bound.
        let quantum = (f64::from(sw) / f64::from(w)).min(f64::from(sh) / f64::from(h)).ceil() as i32;
        prop_assert!((i32::from(first[0]) + i32::from(last[0]) - (sw as i32 - 1)).abs() <= 4 * quantum + 1);
        prop_assert!((i32::from(first[1]) + i32::from(last[1]) - (sh as i32 - 1)).abs() <= 4 * quantum + 1);
        for y in 0..h { for x in 0..w {
            let pixel = sample_cover(&frame,w,h,x,y).unwrap();
            prop_assert!(u32::from(pixel[0]) < sw && u32::from(pixel[1]) < sh);
        }}
    }

    #[test]
    fn overlay_regions_stay_inside_the_surface(
        width in 1u32..=2048, height in 1u32..=2048, scale in 1u32..=4, density in 1u32..=4,
        top in (1u32..=256, 1u32..=256, 0u32..=400), bottom in (1u32..=256, 1u32..=256, 0u32..=400),
        gap in 0u32..=64, margin in 0u32..=256, minimum in 0u32..=100,
    ) {
        let anchored = |(w, h, offset): (u32, u32, u32)| {
            AnchoredOverlay::new(RgbaFrame::new(w, h, vec![0; (w * h * 4) as usize].into()).unwrap(), offset)
        };
        let frame = PresentationFrame::new(
            None,
            OverlayLayout::new(density).unwrap().with_top_fit(gap, margin, minimum),
            anchored(top),
            anchored(bottom),
        );
        let regions = overlay_regions(&frame, width, height, scale);
        for region in regions.into_iter().flatten() {
            prop_assert!(region.width > 0 && region.height > 0);
            prop_assert!(region.x + region.width <= width);
            prop_assert!(region.y + region.height <= height);
            prop_assert_eq!(region.x, (width - region.width) / 2, "overlays are centered");
        }
    }
}
