use crate::{
    checks::{
        check_address, check_freeze_authority, check_mint_authority, check_mint_empty,
        check_owner_program, check_token_mint,
    },
    stake_system::StakeSystem,
    validator_system::ValidatorSystem,
    Initialize, InitializeData, LiqPoolInitialize, ID, MAX_REWARD_FEE,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program_pack::Pack, system_program};

use super::State;

impl<'info> Initialize<'info> {
    pub const CREATOR_AUTHORITY: Pubkey = Pubkey::new_from_array([
        130, 33, 92, 198, 248, 0, 48, 210, 221, 172, 150, 104, 107, 227, 44, 217, 3, 61, 74, 58,
        179, 76, 35, 104, 39, 67, 130, 92, 93, 25, 180, 107,
    ]); // 9kyWPBeU9RnjxnWkkYKYVeShAwQgPDmxujr77thREZtN

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn state_address(&self) -> &Pubkey {
        self.state.to_account_info().key
    }

    fn check_state(&self) -> ProgramResult {
        // all checked by anchor
        Ok(())
    }

    fn check_reserve_pda(&mut self) -> ProgramResult {
        check_owner_program(&self.reserve_pda, &system_program::ID, "reserve_pda")?;
        let (address, bump) = State::find_reserve_address(self.state_address());
        check_address(self.reserve_pda.key, &address, "reserve_pda")?;
        self.state.reserve_bump_seed = bump;
        {
            let lamports = self.reserve_pda.lamports();
            if lamports != self.state.rent_exempt_for_token_acc {
                msg!(
                    "Invalid initial reserve lamports {} expected {}",
                    lamports,
                    self.state.rent_exempt_for_token_acc
                );
                return Err(ProgramError::InvalidArgument);
            }
        }
        Ok(())
    }

    fn check_msol_mint(&mut self) -> ProgramResult {
        check_owner_program(&self.msol_mint, &spl_token::ID, "msol_mint")?;
        let (authority_address, authority_bump_seed) =
            State::find_msol_mint_authority(self.state_address());

        check_mint_authority(&self.msol_mint, authority_address, "msol_mint")?;
        self.state.msol_mint_authority_bump_seed = authority_bump_seed;
        check_mint_empty(&self.msol_mint, "msol_mint")?;
        check_freeze_authority(&self.msol_mint, "msol_mint")?;
        Ok(())
    }

    fn check_treasury_accounts(&self) -> ProgramResult {
        /* check_owner_program(
            &self.treasury_sol_account,
            &system_program::ID,
            "treasury_sol_account",
        )?;*/
        check_owner_program(
            &self.treasury_msol_account,
            &anchor_spl::token::ID,
            "treasury_msol_account",
        )?;
        check_token_mint(
            &self.treasury_msol_account,
            *self.msol_mint.to_account_info().key,
            "treasury_msol_account",
        )?;
        Ok(())
    }

    pub fn process(&mut self, data: InitializeData) -> ProgramResult {
        check_address(
            self.creator_authority.key,
            &Initialize::CREATOR_AUTHORITY,
            "creator_authority",
        )?;
        data.reward_fee.check_max(MAX_REWARD_FEE)?;

        self.state.rent_exempt_for_token_acc =
            self.rent.minimum_balance(spl_token::state::Account::LEN);

        self.check_state()?;
        self.check_reserve_pda()?;
        self.check_msol_mint()?;
        self.check_treasury_accounts()?;
        check_owner_program(
            &self.operational_sol_account,
            &system_program::ID,
            "operational_sol",
        )?;
        check_owner_program(&self.stake_list, &ID, "stake_list")?;
        check_owner_program(&self.validator_list, &ID, "validator_list")?;

        self.state.msol_mint = *self.msol_mint.to_account_info().key;
        self.state.admin_authority = data.admin_authority;
        self.state.operational_sol_account = *self.operational_sol_account.key;

        self.state.reward_fee = data.reward_fee;

        self.state.stake_system = StakeSystem::new(
            self.state_address(),
            *self.stake_list.key,
            &mut self.stake_list.data.as_ref().borrow_mut(),
            data.slots_for_stake_delta,
            data.min_stake,
            0,
            data.additional_stake_record_space,
        )?;
        self.state.validator_system = ValidatorSystem::new(
            *self.validator_list.key,
            &mut self.validator_list.data.as_ref().borrow_mut(),
            data.validator_manager_authority,
            data.additional_validator_record_space,
        )?;

        self.state.msol_price = State::PRICE_DENOMINATOR;

        // self.state.treasury_sol_account = *self.treasury_sol_account.to_account_info().key;
        self.state.treasury_msol_account = *self.treasury_msol_account.to_account_info().key;
        self.state.min_deposit = 1; // 1 lamport
        self.state.min_withdraw = 1; // 1 lamport
        self.state.staking_sol_cap = std::u64::MAX; // Unlimited

        LiqPoolInitialize::process(self, data.liq_pool)?;

        Ok(())
    }
}
