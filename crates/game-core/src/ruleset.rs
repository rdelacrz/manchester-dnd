use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RulesetId {
    #[default]
    #[serde(rename = "srd-5.1-cc")]
    Srd5_1,
}

impl RulesetId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Srd5_1 => "srd-5.1-cc",
        }
    }
}

impl fmt::Display for RulesetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub const RULESET: RulesetId = RulesetId::Srd5_1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_has_a_stable_persistence_value() {
        assert_eq!(RULESET.to_string(), "srd-5.1-cc");
        assert_eq!(serde_json::to_string(&RULESET).unwrap(), "\"srd-5.1-cc\"");
    }
}
