#![no_main]

use libfuzzer_sys::fuzz_target;
use manchester_dnd_server::scene_images::fuzz_image_boundaries;

fuzz_target!(|data: &[u8]| {
    if data.len() <= 512 * 1024 {
        fuzz_image_boundaries(data);
    }
});
