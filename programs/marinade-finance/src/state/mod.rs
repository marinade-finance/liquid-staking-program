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
    pub resume_at_epoch: u64,
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

    pub const PAUSE_MAX_EPOCHS: u64 = 12; // 12 epochs is approx 27 days

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
        self.stake_system
            .delayed_unstake_cooling_down
            .checked_add(self.emergency_cooling_down)
            .expect("Total cooling down overflow")
    }

    /// total_active_balance + total_cooling_down + available_reserve_balance
    pub fn total_lamports_under_control(&self) -> u64 {
        self.validator_system
            .total_active_balance
            .checked_add(self.total_cooling_down())
            .expect("Stake balance overflow")
            .checked_add(self.available_reserve_balance) // reserve_pda.lamports() - self.rent_exempt_for_token_acc
            .expect("Total SOLs under control overflow")
    }

    pub fn check_staking_cap(&self, transfering_lamports: u64) -> Result<()> {
        let result_amount = self
            .total_lamports_under_control()
            .checked_add(transfering_lamports)
            .ok_or(error!(MarinadeError::CalculationFailure))?;
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
    pub fn calc_lamports_from_msol_amount(&self, msol_amount: u64) -> Result<u64> {
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
        self.available_reserve_balance = self
            .available_reserve_balance
            .checked_add(amount)
            .expect("reserve balance overflow");
    }

    pub fn on_transfer_from_reserve(&mut self, amount: u64) -> Result<()> {
        self.available_reserve_balance = self
            .available_reserve_balance
            .checked_sub(amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        Ok(())
    }

    pub fn on_msol_mint(&mut self, amount: u64) {
        self.msol_supply = self
            .msol_supply
            .checked_add(amount)
            .expect("msol supply overflow");
    }

    pub fn on_msol_burn(&mut self, amount: u64) -> Result<()> {
        self.msol_supply = self
            .msol_supply
            .checked_sub(amount)
            .ok_or(MarinadeError::CalculationFailure)?;
        Ok(())
    }

    // ---------------
    // EMERGENCY PAUSE
    // ---------------

    // is paused if `MAX_PAUSE_EPOCHS` haven't elapsed starting from the last `pause()`
    // is paused if we haven't reached `resume_at_epoch`
    // as soon as we enter `resume_at_epoch` epoch the contract will self-resume
    pub fn is_paused(&self) -> Result<bool> {
        Ok(Clock::get()?.epoch < self.resume_at_epoch)
    }

    // set resume_at_epoch to current epoch
    pub fn pause(&mut self) -> Result<()> {
        require!(!self.is_paused()?, MarinadeError::AlreadyPaused);
        let epoch = Clock::get()?.epoch;
        // if we just ended the pause, and for the next epoch, can't re-pause
        require_gt!(
            epoch,
            self.resume_at_epoch + 1,
            MarinadeError::TooSoonToRePause
        );
        self.resume_at_epoch = epoch + State::PAUSE_MAX_EPOCHS;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        require!(self.is_paused()?, MarinadeError::NotPaused);
        // set resume_at_epoch to current epoch
        // this unpauses immediately
        // and prevents re-pausing for the current epoch and the next
        self.resume_at_epoch = Clock::get()?.epoch;
        Ok(())
    }

    // returns MarinadeError::ProgramIsPaused if paused
    pub fn check_paused(&self) -> Result<()> {
        if self.is_paused()? {
            err!(MarinadeError::ProgramIsPaused)
        } else {
            Ok(())
        }
    }
}
