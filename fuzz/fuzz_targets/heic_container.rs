#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    genkan::dynamic_wallpaper::heic::fuzzing::container(data);
});
