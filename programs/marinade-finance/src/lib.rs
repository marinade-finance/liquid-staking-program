#![cfg_attr(not(debug_assertions), deny(warnings))]

use anchor_lang::prelude::*;

use error::MarinadeError;

pub mod calc;
pub mod checks;
pub mod error;
pub mod events;
pub mod instructions;
pub mod state;

use instructions::*;

#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;
pub use state::State;

declare_id!("MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD");

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    name: "Marinade Liquid Staking",
    project_url: "https://marinade.finance",
    contacts: "link:https://docs.marinade.finance/marinade-dao,link:https://discord.com/invite/6EtUf4Euu6",
    policy: "https://docs.marinade.finance/marinade-protocol/security",
    preferred_languages: "en",
    source_code: "https://github.com/marinade-finance/liquid-staking-program",
    source_release: "v2.1.0",
    auditors: "https://docs.marinade.finance/marinade-protocol/security/audits"
}

fn check_context<T>(ctx: &Context<T>) -> Result<()> {
    if !check_id(ctx.program_id) {
        return err!(MarinadeError::InvalidProgramId);
    }
    // make sure there are no extra accounts
    if !ctx.remaining_accounts.is_empty() {
        return err!(MarinadeError::UnexpectedAccount);
    }

    Ok(())
}

//-----------------------------------------------------
#[program]
pub mod marinade_finance {

    use super::*;

    //----------------------------------------------------------------------------
    // Base Instructions
    //----------------------------------------------------------------------------
    // Includes: initialization, contract parameters
    // basic user functions: (liquid)stake, liquid-unstake
    // liq-pool: add-liquidity, remove-liquidity
    // Validator list management
    //----------------------------------------------------------------------------

    pub fn initialize(ctx: Context<Initialize>, data: InitializeData) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts
            .process(data, *ctx.bumps.get("reserve_pda").unwrap())?;
        Ok(())
    }

    pub fn change_authority(
        ctx: Context<ChangeAuthority>,
        data: ChangeAuthorityData,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(data)
    }

    pub fn add_validator(ctx: Context<AddValidator>, score: u32) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(score)
    }

    pub fn remove_validator(
        ctx: Context<RemoveValidator>,
        index: u32,
        validator_vote: Pubkey,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(index, validator_vote)
    }

    pub fn set_validator_score(
        ctx: Context<SetValidatorScore>,
        index: u32,
        validator_vote: Pubkey,
        score: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(index, validator_vote, score)
    }

    pub fn config_validator_system(
        ctx: Context<ConfigValidatorSystem>,
        extra_runs: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(extra_runs)
    }

    // deposit AKA stake, AKA deposit_sol
    pub fn deposit(ctx: Context<Deposit>, lamports: u64) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(lamports)
    }

    // SPL stake pool like
    pub fn deposit_stake_account(
        ctx: Context<DepositStakeAccount>,
        validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(validator_index)
    }

    pub fn liquid_unstake(ctx: Context<LiquidUnstake>, msol_amount: u64) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(msol_amount)
    }

    pub fn add_liquidity(ctx: Context<AddLiquidity>, lamports: u64) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(lamports)
    }

    pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, tokens: u64) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(tokens)
    }

    pub fn config_lp(ctx: Context<ConfigLp>, params: ConfigLpParams) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(params)
    }

    pub fn config_marinade(
        ctx: Context<ConfigMarinade>,
        params: ConfigMarinadeParams,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(params)
    }

    //-------------------------------------------------------------------------------------
    // Advanced instructions: deposit-stake-account, Delayed-Unstake
    // backend/bot "crank" related functions:
    // * order_unstake (starts stake-account deactivation)
    // * withdraw (delete & withdraw from a deactivated stake-account)
    // * update (compute stake-account rewards & update mSOL price)
    //-------------------------------------------------------------------------------------

    pub fn order_unstake(ctx: Context<OrderUnstake>, msol_amount: u64) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(msol_amount)
    }

    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process()
    }

    pub fn stake_reserve(ctx: Context<StakeReserve>, validator_index: u32) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(validator_index)
    }

    pub fn update_active(
        ctx: Context<UpdateActive>,
        stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(stake_index, validator_index)
    }
    pub fn update_deactivated(
        ctx: Context<UpdateDeactivated>,
        stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(stake_index, validator_index)
    }

    pub fn deactivate_stake(
        ctx: Context<DeactivateStake>,
        stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(stake_index, validator_index)
    }

    pub fn emergency_unstake(
        ctx: Context<EmergencyUnstake>,
        stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(stake_index, validator_index)
    }

    pub fn partial_unstake(
        ctx: Context<PartialUnstake>,
        stake_index: u32,
        validator_index: u32,
        desired_unstake_amount: u64,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts
            .process(stake_index, validator_index, desired_unstake_amount)
    }

    pub fn merge_stakes(
        ctx: Context<MergeStakes>,
        destination_stake_index: u32,
        source_stake_index: u32,
        validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts
            .process(destination_stake_index, source_stake_index, validator_index)
    }

    pub fn redelegate(
        ctx: Context<ReDelegate>,
        stake_index: u32,
        source_validator_index: u32,
        dest_validator_index: u32,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts
            .process(stake_index, source_validator_index, dest_validator_index)
    }

    // emergency pauses the contract
    pub fn pause(ctx: Context<EmergencyPause>) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.pause()
    }

    // resumes the contract
    pub fn resume(ctx: Context<EmergencyPause>) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.resume()
    }

    // immediate withdraw of an active stake account - feature can be enabled or disable by the DAO
    pub fn withdraw_stake_account(
        ctx: Context<WithdrawStakeAccount>,
        stake_index: u32,
        validator_index: u32,
        msol_amount: u64,
        beneficiary: Pubkey,
    ) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts
            .process(stake_index, validator_index, msol_amount, beneficiary)
    }

    pub fn realloc_validator_list(ctx: Context<ReallocValidatorList>, capacity: u32) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(capacity)
    }

    pub fn realloc_stake_list(ctx: Context<ReallocStakeList>, capacity: u32) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(capacity)
    }

    pub fn finalize_delinquent_upgrade(ctx: Context<FinalizeDelinquentUpgrade>, max_validators: u32) -> Result<()> {
        check_context(&ctx)?;
        ctx.accounts.process(max_validators)
    }
}
