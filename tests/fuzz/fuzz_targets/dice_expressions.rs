#![no_main]

use std::str::FromStr as _;

use libfuzzer_sys::fuzz_target;
use manchester_dnd_core::DiceExpression;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4_096 {
        return;
    }
    if let Ok(value) = std::str::from_utf8(data)
        && let Ok(expression) = DiceExpression::from_str(value)
    {
        let canonical = expression.to_string();
        assert_eq!(DiceExpression::from_str(&canonical), Ok(expression));
        let json = serde_json::to_vec(&expression).expect("valid dice expressions serialize");
        assert_eq!(
            serde_json::from_slice::<DiceExpression>(&json)
                .expect("serialized dice expressions decode"),
            expression
        );
    }
    let _ = serde_json::from_slice::<DiceExpression>(data);
});
