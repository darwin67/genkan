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
}
