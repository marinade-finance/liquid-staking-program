use anchor_lang::AnchorDeserialize;
use anchor_lang::{
    context::CpiContext,
    solana_program::{account_info::AccountInfo, program::invoke_signed, pubkey::Pubkey},
    Accounts, Result,
};
use solana_program::stake::{self, program::ID, state::StakeState};
use std::ops::Deref;

pub fn withdraw<'info>(
    ctx: CpiContext<'_, '_, '_, 'info, Withdraw<'info>>,
    amount: u64,
    custodian: Option<AccountInfo<'info>>,
) -> Result<()> {
    let ix = stake::instruction::withdraw(
        ctx.accounts.stake.key,
        ctx.accounts.withdrawer.key,
        ctx.accounts.to.key,
        amount,
        custodian.as_ref().map(|c| c.key),
    );
    let mut account_infos = vec![
        ctx.accounts.stake,
        ctx.accounts.to,
        ctx.accounts.clock,
        ctx.accounts.stake_history,
        ctx.accounts.withdrawer,
    ];
    if let Some(c) = custodian {
        account_infos.push(c);
    }
    invoke_signed(&ix, &account_infos, ctx.signer_seeds).map_err(Into::into)
}

pub fn deactivate_stake<'info>(
    ctx: CpiContext<'_, '_, '_, 'info, DeactivateStake<'info>>,
) -> Result<()> {
    let ix = stake::instruction::deactivate_stake(ctx.accounts.stake.key, ctx.accounts.staker.key);
    invoke_signed(
        &ix,
        &[ctx.accounts.stake, ctx.accounts.clock, ctx.accounts.staker],
        ctx.signer_seeds,
    )
    .map_err(Into::into)
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    /// CHECK: CPI account
    pub stake: AccountInfo<'info>,
    /// CHECK: CPI account
    pub withdrawer: AccountInfo<'info>,
    /// CHECK: CPI account
    pub to: AccountInfo<'info>,
    /// CHECK: CPI account
    pub clock: AccountInfo<'info>,
    /// CHECK: CPI account
    pub stake_history: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct DeactivateStake<'info> {
    /// CHECK: CPI account
    pub stake: AccountInfo<'info>,
    /// CHECK: CPI account
    pub staker: AccountInfo<'info>,
    /// CHECK: CPI account
    pub clock: AccountInfo<'info>,
}

#[derive(Clone)]
pub struct StakeAccount(StakeState);

impl anchor_lang::AccountDeserialize for StakeAccount {
    fn try_deserialize(buf: &mut &[u8]) -> Result<Self> {
        Self::try_deserialize_unchecked(buf)
    }

    fn try_deserialize_unchecked(buf: &mut &[u8]) -> Result<Self> {
        StakeState::deserialize(buf).map(Self).map_err(Into::into)
    }
}

impl anchor_lang::AccountSerialize for StakeAccount {}

impl anchor_lang::Owner for StakeAccount {
    fn owner() -> Pubkey {
        ID
    }
}

impl Deref for StakeAccount {
    type Target = StakeState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone)]
pub struct Stake;

impl anchor_lang::Id for Stake {
    fn id() -> Pubkey {
        ID
    }
}
