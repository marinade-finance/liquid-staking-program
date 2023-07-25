use crate::{
    checks::check_token_source_account,
    error::MarinadeError,
    events::user::WithdrawStakeAccountEvent,
    state::{stake_system::StakeSystem, validator_system::ValidatorSystem},
    State,
};
use anchor_lang::{
    prelude::*,
    solana_program::{
        program::invoke_signed,
        stake,
        stake::state::{StakeAuthorize, StakeState},
        system_program,
    },
};
use anchor_spl::{
    stake::{Stake, StakeAccount},
    token::{burn, Burn, Mint, Token, TokenAccount},
};

use crate::checks::check_stake_amount_and_validator;

#[derive(Accounts)]
pub struct WithdrawStakeAccount<'info> {
    #[account(mut, has_one = msol_mint)]
    pub state: Box<Account<'info, State>>,

    #[account(mut)]
    pub msol_mint: Box<Account<'info, Mint>>,

    // Note: new stake account withdraw-auth (owner) & staker-auth will be owner of burn_msol_from
    #[account(
        mut,
        token::mint = msol_mint
    )]
    pub burn_msol_from: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub burn_msol_authority: Signer<'info>,

    /// CHECK: manual account processing
    #[account(
        mut,
        address = state.validator_system.validator_list.account,
        constraint = validator_list.data.borrow().as_ref().get(0..8)
            == Some(ValidatorSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidValidatorListDiscriminator,
    )]
    pub validator_list: UncheckedAccount<'info>,

    /// CHECK: manual account processing
    #[account(
        mut,
        address = state.stake_system.stake_list.account,
        constraint = stake_list.data.borrow().as_ref().get(0..8)
            == Some(StakeSystem::DISCRIMINATOR)
            @ MarinadeError::InvalidStakeListDiscriminator,
    )]
    pub stake_list: UncheckedAccount<'info>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_WITHDRAW_SEED
        ],
        bump = state.stake_system.stake_withdraw_bump_seed
    )]
    pub stake_withdraw_authority: UncheckedAccount<'info>,
    /// CHECK: PDA
    #[account(
        seeds = [
            &state.key().to_bytes(),
            StakeSystem::STAKE_DEPOSIT_SEED
        ],
        bump = state.stake_system.stake_deposit_bump_seed
    )]
    pub stake_deposit_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub stake_account: Box<Account<'info, StakeAccount>>,

    #[account(
        init,
        payer = split_stake_rent_payer,
        space = std::mem::size_of::<StakeState>(),
        owner = stake::program::ID,
    )]
    pub split_stake_account: Account<'info, StakeAccount>,
    #[account(
        mut,
        owner = system_program::ID
    )]
    pub split_stake_rent_payer: Signer<'info>,

    pub clock: Sysvar<'info, Clock>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub stake_program: Program<'info, Stake>,
}

impl<'info> WithdrawStakeAccount<'info> {
    pub fn process(
        &mut self,
        stake_index: u32,
        validator_index: u32,
        msol_amount: u64,
    ) -> Result<()> {
        require!(!self.state.paused, MarinadeError::ProgramIsPaused);
        require!(
            self.state.withdraw_stake_account_enabled,
            MarinadeError::WithdrawStakeAccountIsNotEnabled
        );

        // record  for event
        let user_msol_balance = self.burn_msol_from.amount;
        // save msol price source
        let total_virtual_staked_lamports = self.state.total_virtual_staked_lamports();
        let msol_supply = self.state.msol_supply;

        check_token_source_account(
            &self.burn_msol_from,
            self.burn_msol_authority.key,
            msol_amount,
        )
        .map_err(|e| e.with_account_name("burn_msol_from"))?;

        let mut stake = self.state.stake_system.get_checked(
            &self.stake_list.data.as_ref().borrow(),
            stake_index,
            self.stake_account.to_account_info().key,
        )?;
        let last_update_stake_delegation = stake.last_update_delegated_lamports;

        // require the stake is not in emergency_unstake
        require_eq!(
            stake.is_emergency_unstaking,
            0,
            MarinadeError::StakeAccountIsEmergencyUnstaking
        );

        // require stake is active (deactivation_epoch == u64::MAX)
        let delegation = self.stake_account.delegation().ok_or_else(|| {
            error!(MarinadeError::RequiredDelegatedStake).with_account_name("stake_account")
        })?;
        require_eq!(
            delegation.deactivation_epoch,
            std::u64::MAX,
            MarinadeError::RequiredActiveStake
        );

        let mut validator = self
            .state
            .validator_system
            .get(&self.validator_list.data.as_ref().borrow(), validator_index)?;

        // check currently_staked in this account & validator vote-key
        check_stake_amount_and_validator(
            &self.stake_account,
            stake.last_update_delegated_lamports,
            &validator.validator_account,
        )?;

        // compute how many lamport the burned mSOL represents
        let sol_value = self.state.msol_to_sol(msol_amount)?;
        require_gte!(
            sol_value,
            self.state.min_withdraw,
            MarinadeError::WithdrawAmountIsTooLow
        );

        // apply withdraw_stake_account_fee to avoid economical attacks
        // withdraw_stake_account_fee must be >= one epoch staking rewards
        let withdraw_stake_account_fee_lamports =
            self.state.withdraw_stake_account_fee.apply(sol_value);
        // the fee value will be burned but not delivered, thus increasing mSOL value slightly for all mSOL holders
        let split_lamports = sol_value - withdraw_stake_account_fee_lamports;
        // check withdraw amount (new stake account) >= self.state.stake_system.min_stake
        require_gte!(
            split_lamports,
            self.state.stake_system.min_stake,
            MarinadeError::WithdrawStakeLamportsIsTooLow
        );
        // require remainder stake also >= self.state.stake_system.min_stake
        // To simplify the flow, we always deliver the lamports in the splitted account,
        // so some lamports must remain in the original account. Check that
        // after split, the amount remaining in the stake account is >= state.stake_system.min_stake
        require_gte!(
            stake
                .last_update_delegated_lamports
                .saturating_sub(split_lamports),
            self.state.stake_system.min_stake,
            MarinadeError::StakeAccountRemainderTooLow
        );

        // burn mSOL
        burn(
            CpiContext::new(
                self.token_program.to_account_info(),
                Burn {
                    mint: self.msol_mint.to_account_info(),
                    from: self.burn_msol_from.to_account_info(),
                    authority: self.burn_msol_authority.to_account_info(),
                },
            ),
            msol_amount,
        )?;
        self.state.on_msol_burn(msol_amount)?;

        // split split_lamports from stake account into out split_stake_account
        msg!(
            "Split {} lamports from stake {} into {}",
            split_lamports,
            stake.stake_account,
            self.split_stake_account.key(),
        );

        let split_instruction = stake::instruction::split(
            self.stake_account.to_account_info().key,
            self.stake_deposit_authority.key,
            split_lamports,
            &self.split_stake_account.key(),
        )
        .last()
        .unwrap()
        .clone();
        invoke_signed(
            &split_instruction,
            &[
                self.stake_program.to_account_info(),
                self.stake_account.to_account_info(),
                self.split_stake_account.to_account_info(),
                self.stake_deposit_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_DEPOSIT_SEED,
                &[self.state.stake_system.stake_deposit_bump_seed],
            ]],
        )?;

        stake.last_update_delegated_lamports -= split_lamports;

        // we now consider amount no longer "active" for this specific validator
        validator.active_balance -= split_lamports;
        // and in state totals,
        self.state.validator_system.total_active_balance -= split_lamports;

        // update stake-list & validator-list
        self.state.stake_system.set(
            &mut self.stake_list.data.as_ref().borrow_mut(),
            stake_index,
            stake,
        )?;
        self.state.validator_system.set(
            &mut self.validator_list.data.as_ref().borrow_mut(),
            validator_index,
            validator,
        )?;

        // assign user staker and as withdrawer (owner) for the new split_stake_account
        invoke_signed(
            &stake::instruction::authorize(
                self.split_stake_account.to_account_info().key,
                self.stake_withdraw_authority.key,
                self.burn_msol_authority.key,
                StakeAuthorize::Staker,
                None,
            ),
            &[
                self.stake_program.to_account_info(),
                self.stake_account.to_account_info(),
                self.clock.to_account_info(),
                self.stake_withdraw_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_WITHDRAW_SEED,
                &[self.state.stake_system.stake_withdraw_bump_seed],
            ]],
        )?;
        invoke_signed(
            &stake::instruction::authorize(
                self.split_stake_account.to_account_info().key,
                self.stake_withdraw_authority.key,
                self.burn_msol_authority.key,
                StakeAuthorize::Withdrawer,
                None,
            ),
            &[
                self.stake_program.to_account_info(),
                self.stake_account.to_account_info(),
                self.clock.to_account_info(),
                self.stake_withdraw_authority.to_account_info(),
            ],
            &[&[
                &self.state.key().to_bytes(),
                StakeSystem::STAKE_WITHDRAW_SEED,
                &[self.state.stake_system.stake_withdraw_bump_seed],
            ]],
        )?;

        emit!(WithdrawStakeAccountEvent {
            state: self.state.key(),
            epoch: self.clock.epoch,
            stake_index,
            stake: self.stake_account.key(),
            last_update_stake_delegation,
            validator_index,
            validator: validator.validator_account,
            withdrawer: self.burn_msol_authority.key(),
            user_msol_balance,
            msol_burned: msol_amount,
            split_stake: self.split_stake_account.key(),
            split_lamports,
            fee_bp_cents: self.state.withdraw_stake_account_fee.bp_cents,
            total_virtual_staked_lamports,
            msol_supply,
        });

        Ok(())
    }
}
