#![no_main]

use libfuzzer_sys::fuzz_target;
use manchester_dnd_server::{content::fuzz_pack_json, events::fuzz_review_event_markdown};

fuzz_target!(|data: &[u8]| {
    if data.len() > 256 * 1024 {
        return;
    }
    fuzz_pack_json(data);
    fuzz_review_event_markdown(data);
});
