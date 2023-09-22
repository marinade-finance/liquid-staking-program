use crate::{
    calc::{shares_from_value, value_from_shares},
    error::MarinadeError,
    require_lte, ID,
};
use anchor_lang::{
    prelude::*, solana_program::native_token::LAMPORTS_PER_SOL, solana_program::program_pack::Pack,
};
use anchor_spl::token::spl_token;
use std::mem::MaybeUninit;

use self::{liq_pool::LiqPool, stake_system::StakeSystem, validator_system::ValidatorSystem};

pub mod delayed_unstake_ticket;
pub mod fee;
pub mod liq_pool;
pub mod list;
pub mod stake_system;
pub mod validator_system;

pub use fee::Fee;
pub use fee::FeeCents;

#[account]
#[derive(Debug)]
pub struct State {
    pub msol_mint: Pubkey,

    pub admin_authority: Pubkey,

    // Target for withdrawing rent reserve SOLs. Save bot wallet account here
    pub operational_sol_account: Pubkey,
    // treasury - external accounts managed by marinade DAO
    // pub treasury_sol_account: Pubkey,
    pub treasury_msol_account: Pubkey,

    // Bump seeds:
    pub reserve_bump_seed: u8,
    pub msol_mint_authority_bump_seed: u8,

    pub rent_exempt_for_token_acc: u64, // Token-Account For rent exempt

    // fee applied on rewards
    pub reward_fee: Fee,

    pub stake_system: StakeSystem,
    pub validator_system: ValidatorSystem, //includes total_balance = total stake under management

    // sum of all the orders received in this epoch
    // must not be used for stake-unstake amount calculation
    // only for reference
    // epoch_stake_orders: u64,
    // epoch_unstake_orders: u64,
    pub liq_pool: LiqPool,
    pub available_reserve_balance: u64, // reserve_pda.lamports() - self.rent_exempt_for_token_acc. Virtual value (real may be > because of transfers into reserve). Use Update* to align
    pub msol_supply: u64, // Virtual value (may be < because of token burn). Use Update* to align
    // For FE. Don't use it for token amount calculation
    pub msol_price: u64,

    ///count tickets for delayed-unstake
    pub circulating_ticket_count: u64,
    ///total lamports amount of generated and not claimed yet tickets
    pub circulating_ticket_balance: u64,
    pub lent_from_reserve: u64,
    pub min_deposit: u64,
    pub min_withdraw: u64,
    pub staking_sol_cap: u64,

    pub emergency_cooling_down: u64,

    /// emergency pause
    pub pause_authority: Pubkey,
    pub paused: bool,

    // delayed unstake account fee
    // to avoid economic attacks this value should not be zero
    // (this is required because tickets are ready at the end of the epoch)
    // preferred value is one epoch rewards
    pub delayed_unstake_fee: FeeCents,

    // withdraw stake account fee
    // to avoid economic attacks this value should not be zero
    // (this is required because stake accounts are delivered immediately)
    // preferred value is one epoch rewards
    pub withdraw_stake_account_fee: FeeCents,
    pub withdraw_stake_account_enabled: bool,

    // Limit moving stakes from one validator to another
    // by calling redelegate, emergency_unstake and partial_unstake
    // in case of stolen validator manager key or broken delegation strategy bot
    pub last_stake_move_epoch: u64, // epoch of the last stake move action
    pub stake_moved: u64,           // total amount of moved SOL during the epoch #stake_move_epoch
    pub max_stake_moved_per_epoch: Fee, // % of total_lamports_under_control
}

impl State {
    pub const PRICE_DENOMINATOR: u64 = 0x1_0000_0000;
    /// Suffix for reserve account seed
    pub const RESERVE_SEED: &'static [u8] = b"reserve";
    pub const MSOL_MINT_AUTHORITY_SEED: &'static [u8] = b"st_mint";

    // Account seeds for simplification of creation (optional)
    pub const STAKE_LIST_SEED: &'static str = "stake_list";
    pub const VALIDATOR_LIST_SEED: &'static str = "validator_list";

    pub const MAX_REWARD_FEE: Fee = Fee::from_basis_points(1_000); // 10% max reward fee
    pub const MAX_WITHDRAW_ATOM: u64 = LAMPORTS_PER_SOL / 10;

    // Note as of July 2023, observable staking reward per epoch is 0.045%
    // 1.00045 ** 160 - 1 = 0.0746 ~ 7.46 % which is normal APY for July 2023
    // set a max fee to protect users
    pub const MAX_DELAYED_UNSTAKE_FEE: FeeCents = FeeCents::from_bp_cents(2000); // 0.2% max fee
    pub const MAX_WITHDRAW_STAKE_ACCOUNT_FEE: FeeCents = FeeCents::from_bp_cents(2000); // 0.2% max fee

    // min_stake minimum value is MIN_STAKE_MULTIPLIER * rent_exempt_for_token_acc
    pub const MIN_STAKE_LOWER_LIMIT : u64 = LAMPORTS_PER_SOL / 100;

    pub fn serialized_len() -> usize {
        unsafe { MaybeUninit::<Self>::zeroed().assume_init() }
            .try_to_vec()
            .unwrap()
            .len()
            + 8
    }

    pub fn find_msol_mint_authority(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[&state.to_bytes()[..32], State::MSOL_MINT_AUTHORITY_SEED],
            &ID,
        )
    }

    pub fn find_reserve_address(state: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&state.to_bytes()[..32], Self::RESERVE_SEED], &ID)
    }

    pub fn default_stake_list_address(state: &Pubkey) -> Pubkey {
        Pubkey::create_with_seed(state, Self::STAKE_LIST_SEED, &ID).unwrap()
    }

    pub fn default_validator_list_address(state: &Pubkey) -> Pubkey {
        Pubkey::create_with_seed(state, Self::VALIDATOR_LIST_SEED, &ID).unwrap()
    }

    // this fn returns Some(u64) if the treasury account is valid and ready to receive transfers
    // or None if it is not. This fn does not fail on an invalid treasury account, an invalid
    // treasury account configured in State means the protocol does not want to receive fees
    pub fn get_treasury_msol_balance<'info>(
        &self,
        treasury_msol_account: &AccountInfo<'info>,
    ) -> Option<u64> {
        if treasury_msol_account.owner != &spl_token::ID {
            msg!(
                "treasury_msol_account {} is not a token account",
                treasury_msol_account.key
            );
            return None; // Not an error. Admins may decide to reject fee transfers to themselves
        }

        match spl_token::state::Account::unpack(treasury_msol_account.data.borrow().as_ref()) {
            Ok(token_account) => {
                if token_account.mint == self.msol_mint {
                    Some(token_account.amount)
                } else {
                    msg!(
                        "treasury_msol_account {} has wrong mint {}. Expected {}",
                        treasury_msol_account.key,
                        token_account.mint,
                        self.msol_mint
                    );
                    None // Not an error. Admins may decide to reject fee transfers to themselves
                }
            }
            Err(e) => {
                msg!(
                    "treasury_msol_account {} can not be parsed as token account ({})",
                    treasury_msol_account.key,
                    e
                );
                None // Not an error. Admins may decide to reject fee transfers to themselves
            }
        }
    }

    pub fn total_cooling_down(&self) -> u64 {
        self.stake_system.delayed_unstake_cooling_down + self.emergency_cooling_down
    }

    /// total_active_balance + total_cooling_down + available_reserve_balance
    pub fn total_lamports_under_control(&self) -> u64 {
        self.validator_system.total_active_balance
            + self.total_cooling_down()
            + self.available_reserve_balance // reserve_pda.lamports() - self.rent_exempt_for_token_acc
    }

    pub fn check_staking_cap(&self, transfering_lamports: u64) -> Result<()> {
        let result_amount = self.total_lamports_under_control() + transfering_lamports;
        require_lte!(
            result_amount,
            self.staking_sol_cap,
            MarinadeError::StakingIsCapped
        );
        Ok(())
    }

    pub fn total_virtual_staked_lamports(&self) -> u64 {
        // if we get slashed it may be negative but we must use 0 instead
        self.total_lamports_under_control()
            .saturating_sub(self.circulating_ticket_balance) //tickets created -> cooling down lamports or lamports already in reserve and not claimed yet
    }

    /// calculate the amount of msol tokens corresponding to certain lamport amount
    pub fn calc_msol_from_lamports(&self, stake_lamports: u64) -> Result<u64> {
        shares_from_value(
            stake_lamports,
            self.total_virtual_staked_lamports(),
            self.msol_supply,
        )
    }
    /// calculate lamports value from some msol_amount
    /// result_lamports = msol_amount * msol_price
    pub fn msol_to_sol(&self, msol_amount: u64) -> Result<u64> {
        value_from_shares(
            msol_amount,
            self.total_virtual_staked_lamports(),
            self.msol_supply,
        )
    }

    // **i128**: when do staking/unstaking use real reserve balance instead of virtual field
    pub fn stake_delta(&self, reserve_balance: u64) -> i128 {
        // Never try to stake lamports from emergency_cooling_down
        // (we must wait for update-deactivated first to keep SOLs for claiming on reserve)
        // But if we need to unstake without counting emergency_cooling_down and we have emergency cooling down
        // then we can count part of emergency stakes as starting to cooling down delayed unstakes
        // preventing unstake duplication by recalculating stake-delta for negative values

        // OK. Lets get stake_delta without emergency first
        let raw = reserve_balance.saturating_sub(self.rent_exempt_for_token_acc) as i128
            + self.stake_system.delayed_unstake_cooling_down as i128
            - self.circulating_ticket_balance as i128;
        if raw >= 0 {
            // When it >= 0 it is right value to use
            raw
        } else {
            // Otherwise try to recalculate it with emergency
            let with_emergency = raw + self.emergency_cooling_down as i128;
            // And make sure it will not become positive
            with_emergency.min(0)
        }
    }

    pub fn on_transfer_to_reserve(&mut self, amount: u64) {
        self.available_reserve_balance += amount
    }

    pub fn on_transfer_from_reserve(&mut self, amount: u64) {
        self.available_reserve_balance -= amount
    }

    pub fn on_msol_mint(&mut self, amount: u64) {
        self.msol_supply += amount
    }

    pub fn on_msol_burn(&mut self, amount: u64) {
        self.msol_supply -= amount
    }

    pub fn on_stake_moved(&mut self, amount: u64, clock: &Clock) -> Result<()> {
        if clock.epoch != self.last_stake_move_epoch {
            self.last_stake_move_epoch = clock.epoch;
            self.stake_moved = 0;
        }
        self.stake_moved += amount;
        require_lte!(
            self.stake_moved,
            self.max_stake_moved_per_epoch
                .apply(self.total_lamports_under_control()),
            MarinadeError::MovingStakeIsCapped
        );
        Ok(())
    }
}
