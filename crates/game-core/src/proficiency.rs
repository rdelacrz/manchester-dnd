use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proficiency {
    #[default]
    None,
    Proficient,
    Expertise,
}

impl Proficiency {
    pub const fn multiplier(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Proficient => 1,
            Self::Expertise => 2,
        }
    }

    pub const fn bonus(self, proficiency_bonus: u8) -> u8 {
        proficiency_bonus * self.multiplier()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expertise_doubles_but_does_not_stack_proficiency() {
        assert_eq!(Proficiency::None.bonus(4), 0);
        assert_eq!(Proficiency::Proficient.bonus(4), 4);
        assert_eq!(Proficiency::Expertise.bonus(4), 8);
    }
}
