use crate::{
    checks::{
        check_address, check_freeze_authority, check_mint_authority, check_mint_empty,
        check_token_mint, check_token_owner,
    },
    state::{liq_pool::LiqPool, stake_system::StakeSystem, validator_system::ValidatorSystem, Fee},
    State, ID, MAX_REWARD_FEE,
};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_spl::token::{spl_token, Mint, TokenAccount};

#[derive(Accounts)]
pub struct Initialize<'info> {
    pub creator_authority: Signer<'info>,
    #[account(zero, rent_exempt = enforce)]
    pub state: Box<Account<'info, State>>,

    #[account(seeds = [&state.key().to_bytes(), State::RESERVE_SEED], bump )]
    pub reserve_pda: SystemAccount<'info>,

    /// CHECK: Manual account data management (fixed item size list)
    #[account(mut, rent_exempt = enforce, owner = ID)]
    pub stake_list: UncheckedAccount<'info>,

    /// CHECK: Manual account data management (fixed item size list)
    #[account(mut, rent_exempt = enforce, owner = ID)]
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
    pub reward_fee: Fee,

    pub liq_pool: LiqPoolInitializeData,
    pub additional_stake_record_space: u32,
    pub additional_validator_record_space: u32,
    pub slots_for_stake_delta: u64,
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

    fn check_reserve_pda(&mut self) -> Result<()> {
        let lamports = self.reserve_pda.lamports();
        if lamports != self.state.rent_exempt_for_token_acc {
            msg!(
                "Invalid initial reserve lamports {} expected {}",
                lamports,
                self.state.rent_exempt_for_token_acc
            );
            return Err(Error::from(ProgramError::InvalidArgument));
        }
        Ok(())
    }

    fn check_msol_mint(&mut self) -> Result<()> {
        let (authority_address, authority_bump_seed) =
            State::find_msol_mint_authority(self.state_address());

        check_mint_authority(&self.msol_mint, authority_address, "msol_mint")?;
        self.state.msol_mint_authority_bump_seed = authority_bump_seed;
        check_mint_empty(&self.msol_mint, "msol_mint")?;
        check_freeze_authority(&self.msol_mint, "msol_mint")?;
        Ok(())
    }

    pub fn process(&mut self, data: InitializeData, reserve_pda_bump: u8) -> Result<()> {
        check_address(
            self.creator_authority.key,
            &Initialize::CREATOR_AUTHORITY,
            "creator_authority",
        )?;
        data.reward_fee.check_max(MAX_REWARD_FEE)?;

        self.state.rent_exempt_for_token_acc =
            self.rent.minimum_balance(spl_token::state::Account::LEN);

        self.state.reserve_bump_seed = reserve_pda_bump;
        self.check_reserve_pda()?;
        self.check_msol_mint()?;

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

impl<'info> LiqPoolInitialize<'info> {
    pub fn check_liq_mint(parent: &mut Initialize) -> Result<()> {
        if parent.liq_pool.lp_mint.to_account_info().key == parent.msol_mint.to_account_info().key {
            msg!("Use different mints for stake and liquidity pool");
            return Err(Error::from(ProgramError::InvalidAccountData).with_source(source!()));
        }
        let (authority_address, authority_bump_seed) =
            LiqPool::find_lp_mint_authority(parent.state_address());

        check_mint_authority(&parent.liq_pool.lp_mint, authority_address, "lp_mint")?;

        parent.state.liq_pool.lp_mint_authority_bump_seed = authority_bump_seed;

        check_mint_empty(&parent.liq_pool.lp_mint, "lp_mint")?;
        check_freeze_authority(&parent.liq_pool.lp_mint, "lp_mint")?;

        Ok(())
    }

    pub fn check_sol_account_pda(parent: &mut Initialize) -> Result<()> {
        let (address, bump) = LiqPool::find_sol_leg_address(parent.state_address());
        check_address(
            parent.liq_pool.sol_leg_pda.key,
            &address,
            "liq_sol_account_pda",
        )?;
        parent.state.liq_pool.sol_leg_bump_seed = bump;
        {
            let lamports = parent.liq_pool.sol_leg_pda.lamports();
            if lamports != parent.state.rent_exempt_for_token_acc {
                msg!(
                    "Invalid initial liq_sol_account_pda lamports {} expected {}",
                    lamports,
                    parent.state.rent_exempt_for_token_acc
                );
                return Err(Error::from(ProgramError::InvalidArgument).with_source(source!()));
            }
        }
        Ok(())
    }

    pub fn check_msol_account(parent: &mut Initialize) -> Result<()> {
        check_token_mint(
            &parent.liq_pool.msol_leg,
            *parent.msol_mint.to_account_info().key,
            "liq_msol",
        )?;
        let (msol_authority, msol_authority_bump_seed) =
            LiqPool::find_msol_leg_authority(parent.state_address());
        check_token_owner(&parent.liq_pool.msol_leg, &msol_authority, "liq_msol_leg")?;
        parent.state.liq_pool.msol_leg_authority_bump_seed = msol_authority_bump_seed;
        Ok(())
    }

    pub fn process(parent: &mut Initialize, data: LiqPoolInitializeData) -> Result<()> {
        Self::check_liq_mint(parent)?;
        Self::check_sol_account_pda(parent)?;
        Self::check_msol_account(parent)?;

        parent.state.liq_pool.lp_mint = *parent.liq_pool.lp_mint.to_account_info().key;

        parent.state.liq_pool.msol_leg = *parent.liq_pool.msol_leg.to_account_info().key;

        parent.state.liq_pool.treasury_cut = data.lp_treasury_cut; // Fee { basis_points: 2500 }; //25% from the liquid-unstake fee

        parent.state.liq_pool.lp_liquidity_target = data.lp_liquidity_target; //10_000 SOL
        parent.state.liq_pool.lp_min_fee = data.lp_min_fee; // Fee { basis_points: 30 }; //0.3%
        parent.state.liq_pool.lp_max_fee = data.lp_max_fee; // Fee { basis_points: 300 }; //3%
        parent.state.liq_pool.liquidity_sol_cap = std::u64::MAX; // Unlimited

        parent.state.liq_pool.check_fees()?;

        Ok(())
    }
}
