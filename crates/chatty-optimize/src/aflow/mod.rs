//! AFlow search scaffolding (AGE-13). Operators + MCTS shell are agent-owned;
//! `soft_mixed_select` is reserved.

pub mod select;

/// Named AFlow operators (Generate, Format, Review&Revise, Ensemble, Test, Programmer, Custom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorKind {
    Generate,
    Format,
    ReviewAndRevise,
    Ensemble,
    Test,
    Programmer,
    Custom,
}

impl OperatorKind {
    pub fn all() -> &'static [OperatorKind] {
        &[
            Self::Generate,
            Self::Format,
            Self::ReviewAndRevise,
            Self::Ensemble,
            Self::Test,
            Self::Programmer,
            Self::Custom,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_operators() {
        assert_eq!(OperatorKind::all().len(), 7);
    }
}
