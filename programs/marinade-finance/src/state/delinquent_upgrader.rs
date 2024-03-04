use anchor_lang::prelude::*;

#[derive(Debug, Clone, AnchorSerialize, AnchorDeserialize)]
pub enum DelinquentUpgraderState {
    IteratingStakes {
        visited_count: u32,
        total_active_balance: u64,
        total_delinquent_balance: u64,
    },
    IteratingValidators {
        visited_count: u32,
        delinquent_balance_left: u64,
    },
    Done,
}

impl Default for DelinquentUpgraderState {
    fn default() -> Self {
        Self::IteratingStakes {
            visited_count: 0,
            total_active_balance: 0,
            total_delinquent_balance: 0,
        }
    }
}

impl DelinquentUpgraderState {
    pub fn is_iterating_stakes(&self) -> bool {
        matches!(self, Self::IteratingStakes { .. })
    }
    
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}