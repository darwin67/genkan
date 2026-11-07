#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    genkan::sessions::fuzz_parse(data);
});
