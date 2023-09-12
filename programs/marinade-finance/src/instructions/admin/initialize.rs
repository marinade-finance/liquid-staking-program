use crate::{
    checks::{
        check_freeze_authority, check_mint_authority, check_mint_empty, check_token_mint,
        check_token_owner,
    },
    error::MarinadeError,
    events::admin::InitializeEvent,
    require_lte,
    state::{
        fee::FeeCents, liq_pool::LiqPool, stake_system::StakeSystem,
        validator_system::ValidatorSystem, Fee,
    },
    State, ID,
};
use anchor_lang::{prelude::*, solana_program::program_pack::Pack};
use anchor_spl::token::{spl_token, Mint, TokenAccount};

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(zero)]
    pub state: Box<Account<'info, State>>,

    #[account(
        seeds = [
            &state.key().to_bytes(),
            State::RESERVE_SEED
        ],
        bump,
    )]
    pub reserve_pda: SystemAccount<'info>,

    /// CHECK: Manual account data management (fixed item size list)
    #[account(
        zero,
        owner = ID,
    )]
    pub stake_list: UncheckedAccount<'info>,

    /// CHECK: Manual account data management (fixed item size list)
    #[account(
        zero,
        owner = ID,
    )]
    pub validator_list: UncheckedAccount<'info>,

    pub msol_mint: Box<Account<'info, Mint>>,

    pub operational_sol_account: SystemAccount<'info>,

    pub liq_pool: LiqPoolInitialize<'info>,

    #[account(token::mint = msol_mint)]
    pub treasury_msol_account: Box<Account<'info, TokenAccount>>,

    pub clock: Sysvar<'info, Clock>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct InitializeData {
    pub admin_authority: Pubkey,
    pub validator_manager_authority: Pubkey,
    pub min_stake: u64,
    pub rewards_fee: Fee,

    pub liq_pool: LiqPoolInitializeData,
    pub additional_stake_record_space: u32,
    pub additional_validator_record_space: u32,
    pub slots_for_stake_delta: u64,
    pub pause_authority: Pubkey,
}

#[derive(Accounts)]
pub struct LiqPoolInitialize<'info> {
    pub lp_mint: Box<Account<'info, Mint>>,
    pub sol_leg_pda: SystemAccount<'info>,
    pub msol_leg: Box<Account<'info, TokenAccount>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, AnchorSerialize, AnchorDeserialize)]
pub struct LiqPoolInitializeData {
    pub lp_liquidity_target: u64,
    pub lp_max_fee: Fee,
    pub lp_min_fee: Fee,
    pub lp_treasury_cut: Fee,
}

impl<'info> Initialize<'info> {
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn state_address(&self) -> &Pubkey {
        self.state.to_account_info().key
    }

    fn check_reserve_pda(&mut self, required_lamports: u64) -> Result<()> {
        require_eq!(self.reserve_pda.lamports(), required_lamports);
        Ok(())
    }

    fn check_msol_mint(&mut self) -> Result<u8> {
        let (authority_address, authority_bump_seed) =
            State::find_msol_mint_authority(self.state_address());

        check_mint_authority(&self.msol_mint, &authority_address, "msol_mint")?;
        check_mint_empty(&self.msol_mint, "msol_mint")?;
        check_freeze_authority(&self.msol_mint, "msol_mint")?;
        Ok(authority_bump_seed)
    }

    pub fn process(
        &mut self,
        InitializeData {
            admin_authority,
            validator_manager_authority,
            min_stake,
            rewards_fee,
            liq_pool,
            additional_stake_record_space,
            additional_validator_record_space,
            slots_for_stake_delta,
            pause_authority,
        }: InitializeData,
        reserve_pda_bump: u8,
    ) -> Result<()> {
        require_lte!(
            rewards_fee,
            State::MAX_REWARD_FEE,
            MarinadeError::RewardsFeeIsTooHigh
        );
        require_keys_neq!(self.state.key(), self.stake_list.key());
        require_keys_neq!(self.state.key(), self.validator_list.key());
        require_keys_neq!(self.stake_list.key(), self.validator_list.key());
        let rent_exempt_for_token_acc = self.rent.minimum_balance(spl_token::state::Account::LEN);
        self.check_reserve_pda(rent_exempt_for_token_acc)?;
        let msol_mint_authority_bump_seed = self.check_msol_mint()?;
        self.state.set_inner(State {
            msol_mint: *self.msol_mint.to_account_info().key,
            admin_authority,
            operational_sol_account: *self.operational_sol_account.key,
            treasury_msol_account: *self.treasury_msol_account.to_account_info().key,
            reserve_bump_seed: reserve_pda_bump,
            msol_mint_authority_bump_seed,
            rent_exempt_for_token_acc,
            reward_fee: rewards_fee,
            stake_system: StakeSystem::new(
                self.state_address(),
                *self.stake_list.key,
                &mut self.stake_list.data.as_ref().borrow_mut(),
                slots_for_stake_delta,
                min_stake,
                0,
                additional_stake_record_space,
            )?,
            validator_system: ValidatorSystem::new(
                *self.validator_list.key,
                &mut self.validator_list.data.as_ref().borrow_mut(),
                validator_manager_authority,
                additional_validator_record_space,
            )?,
            liq_pool: LiqPoolInitialize::process(self, liq_pool, rent_exempt_for_token_acc)?,
            available_reserve_balance: 0,
            msol_supply: 0,
            msol_price: State::PRICE_DENOMINATOR,
            circulating_ticket_count: 0,
            circulating_ticket_balance: 0,
            lent_from_reserve: 0,
            min_deposit: 1,                 // 1 lamport
            min_withdraw: 1,                // 1 lamport
            staking_sol_cap: std::u64::MAX, // Unlimited
            emergency_cooling_down: 0,
            pause_authority,
            paused: false,
            delayed_unstake_fee: FeeCents::from_bp_cents(0),
            withdraw_stake_account_fee: FeeCents::from_bp_cents(0),
            withdraw_stake_account_enabled: false,
        });

        emit!(InitializeEvent {
            state: self.state.key(),
            params: InitializeData {
                admin_authority,
                validator_manager_authority,
                min_stake,
                rewards_fee,
                liq_pool,
                additional_stake_record_space,
                additional_validator_record_space,
                slots_for_stake_delta,
                pause_authority
            },
            stake_list: self.stake_list.key(),
            validator_list: self.validator_list.key(),
            msol_mint: self.msol_mint.key(),
            operational_sol_account: self.operational_sol_account.key(),
            lp_mint: self.liq_pool.lp_mint.key(),
            lp_msol_leg: self.liq_pool.msol_leg.key(),
            treasury_msol_account: self.treasury_msol_account.key(),
        });

        Ok(())
    }
}

impl<'info> LiqPoolInitialize<'info> {
    pub fn check_lp_mint(parent: &Initialize) -> Result<u8> {
        require_keys_neq!(parent.liq_pool.lp_mint.key(), parent.msol_mint.key(),);
        let (authority_address, authority_bump_seed) =
            LiqPool::find_lp_mint_authority(parent.state_address());

        check_mint_authority(&parent.liq_pool.lp_mint, &authority_address, "lp_mint")?;
        check_mint_empty(&parent.liq_pool.lp_mint, "lp_mint")?;
        check_freeze_authority(&parent.liq_pool.lp_mint, "lp_mint")?;

        Ok(authority_bump_seed)
    }

    pub fn check_sol_leg(parent: &Initialize, required_lamports: u64) -> Result<u8> {
        let (address, bump) = LiqPool::find_sol_leg_address(parent.state_address());
        require_keys_eq!(parent.liq_pool.sol_leg_pda.key(), address);
        require_eq!(parent.liq_pool.sol_leg_pda.lamports(), required_lamports);
        Ok(bump)
    }

    pub fn check_msol_leg(parent: &Initialize) -> Result<u8> {
        check_token_mint(
            &parent.liq_pool.msol_leg,
            &parent.msol_mint.key(),
            "liq_msol",
        )?;
        let (msol_authority, msol_authority_bump_seed) =
            LiqPool::find_msol_leg_authority(parent.state_address());
        check_token_owner(&parent.liq_pool.msol_leg, &msol_authority, "liq_msol_leg")?;
        Ok(msol_authority_bump_seed)
    }

    pub fn process(
        parent: &Initialize,
        LiqPoolInitializeData {
            lp_liquidity_target,
            lp_max_fee,
            lp_min_fee,
            lp_treasury_cut,
        }: LiqPoolInitializeData,
        required_sol_leg_lamports: u64,
    ) -> Result<LiqPool> {
        let lp_mint_authority_bump_seed = Self::check_lp_mint(parent)?;
        let sol_leg_bump_seed = Self::check_sol_leg(parent, required_sol_leg_lamports)?;
        let msol_leg_authority_bump_seed = Self::check_msol_leg(parent)?;
        let liq_pool = LiqPool {
            lp_mint: *parent.liq_pool.lp_mint.to_account_info().key,
            lp_mint_authority_bump_seed,
            sol_leg_bump_seed,
            msol_leg_authority_bump_seed,
            msol_leg: *parent.liq_pool.msol_leg.to_account_info().key,
            lp_liquidity_target,
            lp_max_fee,
            lp_min_fee,
            treasury_cut: lp_treasury_cut,
            lp_supply: 0,
            lent_from_sol_leg: 0,
            liquidity_sol_cap: std::u64::MAX,
        };

        liq_pool.validate()?;

        Ok(liq_pool)
    }
}
