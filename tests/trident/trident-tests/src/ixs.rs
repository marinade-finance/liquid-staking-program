//! Instruction builders with a per-slot *role*, so the mutator knows what a substitution is allowed to do.
//!
//! Account metas come from the program's own generated `accounts::*` structs (`ToAccountMetas`), the
//! role spec is a compact string, one token per account in declaration order:
//!
//! * `name:F`        fixed: must be exactly this account (PDA, stored address, program, sysvar). ANY
//!                   substitution that still succeeds is a finding.
//! * `name:A`        authority signer (admin / validator manager / pause authority). Substituting another
//!                   key or dropping the signer flag must fail.
//! * `name:N`        fresh account to be created (`init`): signer, any unused / system wallet key is fine.
//! * `name:U/t1|t2`  free, caller-chosen account. Success is only legitimate if the substitute carries one
//!                   of the tags (see `World::tags`); success with a wrong kind is a finding.
//!
//! Every successful transaction is then checked against the accounting invariants regardless of role.

use crate::util::*;
use anchor_lang::{InstructionData, ToAccountMetas};
use marinade_finance::instructions::admin::{ChangeAuthorityData, ConfigLpParams, ConfigMarinadeParams};
use marinade_finance::{accounts, instruction};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Fixed,
    Auth,
    New,
    Free,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gate {
    None,
    Admin,
    Vmgr,
    Pauser,
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub name: &'static str,
    pub meta: AccountMeta,
    pub role: Role,
    pub accepts: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct Ix {
    pub name: &'static str,
    pub slots: Vec<Slot>,
    pub data: Vec<u8>,
    pub gate: Gate,
    /// extra tx instructions that must run first (none today, reserved)
    pub arg_note: String,
}

impl Ix {
    pub fn to_instruction(&self) -> Instruction {
        Instruction {
            program_id: program_id(),
            accounts: self.slots.iter().map(|s| s.meta.clone()).collect(),
            data: self.data.clone(),
        }
    }
    pub fn slot(&self, name: &str) -> Option<&Slot> {
        self.slots.iter().find(|s| s.name == name)
    }
    pub fn key(&self, name: &str) -> Pubkey {
        self.slot(name).unwrap_or_else(|| panic!("{}: no slot {}", self.name, name)).meta.pubkey
    }
}

fn mk(name: &'static str, metas: Vec<AccountMeta>, data: Vec<u8>, spec: &str, gate: Gate) -> Ix {
    let toks: Vec<&str> = spec.split_whitespace().collect();
    assert_eq!(toks.len(), metas.len(), "{name}: spec has {} slots, program struct has {}", toks.len(), metas.len());
    let slots = toks
        .iter()
        .zip(metas)
        .map(|(tok, meta)| {
            let (n, r) = tok.split_once(':').expect("slot spec name:ROLE");
            let (role, accepts) = match r.split_once('/') {
                Some((r, tags)) => (r, tags.split('|').map(tag).collect()),
                None => (r, vec![]),
            };
            let role = match role {
                "F" => Role::Fixed,
                "A" => Role::Auth,
                "N" => Role::New,
                "U" => Role::Free,
                _ => panic!("bad role {role}"),
            };
            Slot { name: Box::leak(n.to_string().into_boxed_str()), meta, role, accepts }
        })
        .collect();
    Ix { name, slots, data, gate, arg_note: String::new() }
}

fn tag(s: &str) -> &'static str {
    match s {
        "wallet" => "wallet",
        "msol_ta" => "msol_ta",
        "lp_ta" => "lp_ta",
        "ticket" => "ticket",
        "ticket_new" => "ticket_new",
        "zero_list" => "zero_list",
        "msol_ta_C" => "msol_ta_C",
        "mint_C" => "mint_C",
        "any" => "*",
        "stake_user" => "stake_user",
        "vote_reg" => "vote_reg",
        "vote" => "vote",
        "any_vote" => "vote",
        other => panic!("unknown tag {other}"),
    }
}

use crate::world::{Inst, User};

pub fn sys() -> Pubkey {
    system_id()
}

pub fn initialize(a: &Inst, data: marinade_finance::instructions::admin::InitializeData) -> Ix {
    mk(
        "initialize",
        accounts::Initialize {
            state: a.state,
            reserve_pda: a.reserve,
            stake_list: a.stake_list,
            validator_list: a.validator_list,
            msol_mint: a.msol_mint,
            operational_sol_account: a.operational,
            liq_pool: accounts::LiqPoolInitialize { lp_mint: a.lp_mint, sol_leg_pda: a.sol_leg, msol_leg: a.msol_leg },
            treasury_msol_account: a.treasury_msol,
            clock: clock_id(),
            rent: rent_id(),
        }
        .to_account_metas(None),
        instruction::Initialize { data }.data(),
        "state:F reserve:F stake_list:U/zero_list validator_list:U/zero_list msol_mint:U/mint_C operational:U/wallet lp_mint:U/mint_C sol_leg:F msol_leg:U/msol_ta_C treasury_msol:U/msol_ta_C clock:F rent:F",
        Gate::None,
    )
}

pub fn deposit(a: &Inst, from: &Pubkey, mint_to: &Pubkey, lamports: u64) -> Ix {
    mk(
        "deposit",
        accounts::Deposit {
            state: a.state,
            msol_mint: a.msol_mint,
            liq_pool_sol_leg_pda: a.sol_leg,
            liq_pool_msol_leg: a.msol_leg,
            liq_pool_msol_leg_authority: a.msol_leg_auth,
            reserve_pda: a.reserve,
            transfer_from: *from,
            mint_to: *mint_to,
            msol_mint_authority: a.msol_mint_auth,
            system_program: sys(),
            token_program: token_id(),
        }
        .to_account_metas(None),
        instruction::Deposit { lamports }.data(),
        "state:F msol_mint:F sol_leg:F msol_leg:F msol_leg_auth:F reserve:F transfer_from:U/wallet mint_to:U/msol_ta msol_mint_auth:F system:F token:F",
        Gate::None,
    )
}

pub fn liquid_unstake(a: &Inst, msol_from: &Pubkey, authority: &Pubkey, sol_to: &Pubkey, msol_amount: u64) -> Ix {
    mk(
        "liquid_unstake",
        accounts::LiquidUnstake {
            state: a.state,
            msol_mint: a.msol_mint,
            liq_pool_sol_leg_pda: a.sol_leg,
            liq_pool_msol_leg: a.msol_leg,
            treasury_msol_account: a.treasury_msol,
            get_msol_from: *msol_from,
            get_msol_from_authority: *authority,
            transfer_sol_to: *sol_to,
            system_program: sys(),
            token_program: token_id(),
        }
        .to_account_metas(None),
        instruction::LiquidUnstake { msol_amount }.data(),
        "state:F msol_mint:F sol_leg:F msol_leg:F treasury_msol:F get_msol_from:U/msol_ta authority:U/wallet sol_to:U/wallet system:F token:F",
        Gate::None,
    )
}

pub fn add_liquidity(a: &Inst, from: &Pubkey, lp_to: &Pubkey, lamports: u64) -> Ix {
    mk(
        "add_liquidity",
        accounts::AddLiquidity {
            state: a.state,
            lp_mint: a.lp_mint,
            lp_mint_authority: a.lp_mint_auth,
            liq_pool_msol_leg: a.msol_leg,
            liq_pool_sol_leg_pda: a.sol_leg,
            transfer_from: *from,
            mint_to: *lp_to,
            system_program: sys(),
            token_program: token_id(),
        }
        .to_account_metas(None),
        instruction::AddLiquidity { lamports }.data(),
        "state:F lp_mint:F lp_mint_auth:F msol_leg:F sol_leg:F transfer_from:U/wallet mint_to:U/lp_ta system:F token:F",
        Gate::None,
    )
}

pub fn remove_liquidity(a: &Inst, lp_from: &Pubkey, authority: &Pubkey, sol_to: &Pubkey, msol_to: &Pubkey, tokens: u64) -> Ix {
    mk(
        "remove_liquidity",
        accounts::RemoveLiquidity {
            state: a.state,
            lp_mint: a.lp_mint,
            burn_from: *lp_from,
            burn_from_authority: *authority,
            transfer_sol_to: *sol_to,
            transfer_msol_to: *msol_to,
            liq_pool_sol_leg_pda: a.sol_leg,
            liq_pool_msol_leg: a.msol_leg,
            liq_pool_msol_leg_authority: a.msol_leg_auth,
            system_program: sys(),
            token_program: token_id(),
        }
        .to_account_metas(None),
        instruction::RemoveLiquidity { tokens }.data(),
        "state:F lp_mint:F burn_from:U/lp_ta authority:U/wallet sol_to:U/wallet msol_to:U/msol_ta sol_leg:F msol_leg:F msol_leg_auth:F system:F token:F",
        Gate::None,
    )
}

pub fn order_unstake(a: &Inst, msol_from: &Pubkey, authority: &Pubkey, ticket: &Pubkey, msol_amount: u64) -> Ix {
    mk(
        "order_unstake",
        accounts::OrderUnstake {
            state: a.state,
            msol_mint: a.msol_mint,
            burn_msol_from: *msol_from,
            burn_msol_authority: *authority,
            new_ticket_account: *ticket,
            clock: clock_id(),
            rent: rent_id(),
            token_program: token_id(),
        }
        .to_account_metas(None),
        instruction::OrderUnstake { msol_amount }.data(),
        "state:F msol_mint:F burn_from:U/msol_ta authority:U/wallet new_ticket:U/ticket_new clock:F rent:F token:F",
        Gate::None,
    )
}

pub fn claim(a: &Inst, ticket: &Pubkey, beneficiary: &Pubkey) -> Ix {
    mk(
        "claim",
        accounts::Claim {
            state: a.state,
            reserve_pda: a.reserve,
            ticket_account: *ticket,
            transfer_sol_to: *beneficiary,
            clock: clock_id(),
            system_program: sys(),
        }
        .to_account_metas(None),
        instruction::Claim {}.data(),
        "state:F reserve:F ticket:U/ticket sol_to:F clock:F system:F",
        Gate::None,
    )
}

pub fn stake_reserve(a: &Inst, validator_index: u32, vote: &Pubkey, new_stake: &Pubkey, rent_payer: &Pubkey) -> Ix {
    mk(
        "stake_reserve",
        accounts::StakeReserve {
            state: a.state,
            validator_list: a.validator_list,
            stake_list: a.stake_list,
            validator_vote: *vote,
            reserve_pda: a.reserve,
            stake_account: *new_stake,
            stake_deposit_authority: a.deposit_auth,
            rent_payer: *rent_payer,
            clock: clock_id(),
            epoch_schedule: epoch_schedule_id(),
            rent: rent_id(),
            stake_history: stake_history_id(),
            stake_config: stake_config_id(),
            system_program: sys(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::StakeReserve { validator_index }.data(),
        "state:F validator_list:F stake_list:F vote:F reserve:F stake_account:N deposit_auth:F rent_payer:U/wallet clock:F epoch_schedule:F rent:F stake_history:F stake_config:F system:F stake_program:F",
        Gate::None,
    )
}

#[allow(clippy::too_many_arguments)]
fn update_common(a: &Inst, stake: &Pubkey) -> accounts::UpdateCommon {
    accounts::UpdateCommon {
        state: a.state,
        stake_list: a.stake_list,
        stake_account: *stake,
        stake_withdraw_authority: a.withdraw_auth,
        reserve_pda: a.reserve,
        msol_mint: a.msol_mint,
        msol_mint_authority: a.msol_mint_auth,
        treasury_msol_account: a.treasury_msol,
        clock: clock_id(),
        stake_history: stake_history_id(),
        stake_program: stake_id(),
        token_program: token_id(),
        validator_list: a.validator_list,
    }
}

const UPDATE_SPEC: &str =
    "state:F stake_list:F stake_account:F withdraw_auth:F reserve:F msol_mint:F msol_mint_auth:F treasury_msol:F clock:F stake_history:F stake_program:F token:F validator_list:F";

pub fn update_active(a: &Inst, stake: &Pubkey, stake_index: u32, validator_index: u32) -> Ix {
    mk(
        "update_active",
        accounts::UpdateActive { common: update_common(a, stake) }.to_account_metas(None),
        instruction::UpdateActive { stake_index, validator_index }.data(),
        UPDATE_SPEC,
        Gate::None,
    )
}

pub fn update_deactivated(a: &Inst, stake: &Pubkey, stake_index: u32, validator_index: u32) -> Ix {
    mk(
        "update_deactivated",
        accounts::UpdateDeactivated { common: update_common(a, stake), operational_sol_account: a.operational, system_program: sys() }
            .to_account_metas(None),
        instruction::UpdateDeactivated { stake_index, validator_index }.data(),
        &format!("{UPDATE_SPEC} operational:F system:F"),
        Gate::None,
    )
}

pub fn deactivate_stake(a: &Inst, stake: &Pubkey, split: &Pubkey, rent_payer: &Pubkey, stake_index: u32, validator_index: u32) -> Ix {
    mk(
        "deactivate_stake",
        accounts::DeactivateStake {
            state: a.state,
            reserve_pda: a.reserve,
            validator_list: a.validator_list,
            stake_list: a.stake_list,
            stake_account: *stake,
            stake_deposit_authority: a.deposit_auth,
            split_stake_account: *split,
            split_stake_rent_payer: *rent_payer,
            clock: clock_id(),
            rent: rent_id(),
            epoch_schedule: epoch_schedule_id(),
            stake_history: stake_history_id(),
            system_program: sys(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::DeactivateStake { stake_index, validator_index }.data(),
        "state:F reserve:F validator_list:F stake_list:F stake_account:F deposit_auth:F split:N rent_payer:U/wallet clock:F rent:F epoch_schedule:F stake_history:F system:F stake_program:F",
        Gate::None,
    )
}

pub fn merge_stakes(a: &Inst, dest: &Pubkey, source: &Pubkey, dest_index: u32, source_index: u32, validator_index: u32) -> Ix {
    mk(
        "merge_stakes",
        accounts::MergeStakes {
            state: a.state,
            stake_list: a.stake_list,
            validator_list: a.validator_list,
            destination_stake: *dest,
            source_stake: *source,
            stake_deposit_authority: a.deposit_auth,
            stake_withdraw_authority: a.withdraw_auth,
            operational_sol_account: a.operational,
            clock: clock_id(),
            stake_history: stake_history_id(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::MergeStakes { destination_stake_index: dest_index, source_stake_index: source_index, validator_index }.data(),
        "state:F stake_list:F validator_list:F dest:F source:F deposit_auth:F withdraw_auth:F operational:F clock:F stake_history:F stake_program:F",
        Gate::None,
    )
}

pub fn create_canonical_stake(a: &Inst, canonical: &Pubkey, source: &Pubkey, source_index: u32, validator_index: u32) -> Ix {
    mk(
        "create_canonical_stake",
        accounts::CreateCanonicalStake {
            state: a.state,
            stake_list: a.stake_list,
            validator_list: a.validator_list,
            canonical_stake: *canonical,
            source_stake: *source,
            stake_deposit_authority: a.deposit_auth,
            stake_withdraw_authority: a.withdraw_auth,
            operational_sol_account: a.operational,
            clock: clock_id(),
            stake_history: stake_history_id(),
            stake_program: stake_id(),
            system_program: sys(),
        }
        .to_account_metas(None),
        instruction::CreateCanonicalStake { source_stake_index: source_index, validator_index }.data(),
        "state:F stake_list:F validator_list:F canonical:F source:F deposit_auth:F withdraw_auth:F operational:F clock:F stake_history:F stake_program:F system:F",
        Gate::None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn partial_unstake(a: &Inst, stake: &Pubkey, split: &Pubkey, rent_payer: &Pubkey, stake_index: u32, validator_index: u32, desired: u64) -> Ix {
    mk(
        "partial_unstake",
        accounts::PartialUnstake {
            state: a.state,
            validator_manager_authority: a.vmgr,
            validator_list: a.validator_list,
            stake_list: a.stake_list,
            stake_account: *stake,
            stake_deposit_authority: a.deposit_auth,
            reserve_pda: a.reserve,
            split_stake_account: *split,
            split_stake_rent_payer: *rent_payer,
            clock: clock_id(),
            rent: rent_id(),
            stake_history: stake_history_id(),
            system_program: sys(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::PartialUnstake { stake_index, validator_index, desired_unstake_amount: desired }.data(),
        "state:F vmgr:A validator_list:F stake_list:F stake_account:F deposit_auth:F reserve:F split:N rent_payer:U/wallet clock:F rent:F stake_history:F system:F stake_program:F",
        Gate::Vmgr,
    )
}

pub fn emergency_unstake(a: &Inst, stake: &Pubkey, stake_index: u32, validator_index: u32) -> Ix {
    mk(
        "emergency_unstake",
        accounts::EmergencyUnstake {
            state: a.state,
            validator_manager_authority: a.vmgr,
            validator_list: a.validator_list,
            stake_list: a.stake_list,
            stake_account: *stake,
            stake_deposit_authority: a.deposit_auth,
            clock: clock_id(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::EmergencyUnstake { stake_index, validator_index }.data(),
        "state:F vmgr:A validator_list:F stake_list:F stake_account:F deposit_auth:F clock:F stake_program:F",
        Gate::Vmgr,
    )
}

pub fn add_validator(a: &Inst, vote: &Pubkey, flag: &Pubkey, rent_payer: &Pubkey, score: u32) -> Ix {
    mk(
        "add_validator",
        accounts::AddValidator {
            state: a.state,
            manager_authority: a.vmgr,
            validator_list: a.validator_list,
            validator_vote: *vote,
            duplication_flag: *flag,
            rent_payer: *rent_payer,
            clock: clock_id(),
            rent: rent_id(),
            system_program: sys(),
        }
        .to_account_metas(None),
        instruction::AddValidator { score }.data(),
        "state:F vmgr:A validator_list:F vote:U/vote dup_flag:F rent_payer:U/wallet clock:F rent:F system:F",
        Gate::Vmgr,
    )
}

pub fn remove_validator(a: &Inst, index: u32, vote: &Pubkey, flag: &Pubkey) -> Ix {
    mk(
        "remove_validator",
        accounts::RemoveValidator {
            state: a.state,
            manager_authority: a.vmgr,
            validator_list: a.validator_list,
            duplication_flag: *flag,
            operational_sol_account: a.operational,
        }
        .to_account_metas(None),
        instruction::RemoveValidator { index, validator_vote: *vote }.data(),
        "state:F vmgr:A validator_list:F dup_flag:F operational:F",
        Gate::Vmgr,
    )
}

pub fn set_validator_score(a: &Inst, index: u32, vote: &Pubkey, score: u32) -> Ix {
    mk(
        "set_validator_score",
        accounts::SetValidatorScore { state: a.state, manager_authority: a.vmgr, validator_list: a.validator_list }.to_account_metas(None),
        instruction::SetValidatorScore { index, validator_vote: *vote, score }.data(),
        "state:F vmgr:A validator_list:F",
        Gate::Vmgr,
    )
}

pub fn config_validator_system(a: &Inst, extra_runs: u32) -> Ix {
    mk(
        "config_validator_system",
        accounts::ConfigValidatorSystem { state: a.state, manager_authority: a.vmgr }.to_account_metas(None),
        instruction::ConfigValidatorSystem { extra_runs }.data(),
        "state:F vmgr:A",
        Gate::Vmgr,
    )
}

pub fn config_marinade(a: &Inst, params: ConfigMarinadeParams) -> Ix {
    mk(
        "config_marinade",
        accounts::ConfigMarinade { state: a.state, admin_authority: a.admin }.to_account_metas(None),
        instruction::ConfigMarinade { params }.data(),
        "state:F admin:A",
        Gate::Admin,
    )
}

pub fn config_lp(a: &Inst, params: ConfigLpParams) -> Ix {
    mk(
        "config_lp",
        accounts::ConfigLp { state: a.state, admin_authority: a.admin }.to_account_metas(None),
        instruction::ConfigLp { params }.data(),
        "state:F admin:A",
        Gate::Admin,
    )
}

pub fn change_authority(a: &Inst, data: ChangeAuthorityData) -> Ix {
    mk(
        "change_authority",
        accounts::ChangeAuthority { state: a.state, admin_authority: a.admin }.to_account_metas(None),
        instruction::ChangeAuthority { data }.data(),
        "state:F admin:A",
        Gate::Admin,
    )
}

pub fn pause(a: &Inst) -> Ix {
    mk(
        "pause",
        accounts::EmergencyPause { state: a.state, pause_authority: a.pauser }.to_account_metas(None),
        instruction::Pause {}.data(),
        "state:F pauser:A",
        Gate::Pauser,
    )
}

pub fn resume(a: &Inst) -> Ix {
    mk(
        "resume",
        accounts::EmergencyPause { state: a.state, pause_authority: a.pauser }.to_account_metas(None),
        instruction::Resume {}.data(),
        "state:F pauser:A",
        Gate::Pauser,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn deposit_stake_account(a: &Inst, stake: &Pubkey, stake_auth: &Pubkey, flag: &Pubkey, rent_payer: &Pubkey, mint_to: &Pubkey, validator_index: u32) -> Ix {
    mk(
        "deposit_stake_account",
        accounts::DepositStakeAccount {
            state: a.state,
            validator_list: a.validator_list,
            stake_list: a.stake_list,
            stake_account: *stake,
            stake_authority: *stake_auth,
            duplication_flag: *flag,
            rent_payer: *rent_payer,
            msol_mint: a.msol_mint,
            mint_to: *mint_to,
            msol_mint_authority: a.msol_mint_auth,
            clock: clock_id(),
            rent: rent_id(),
            system_program: sys(),
            token_program: token_id(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::DepositStakeAccount { validator_index }.data(),
        "state:F validator_list:F stake_list:F stake_account:U/stake_user stake_authority:U/wallet dup_flag:U/any rent_payer:U/wallet msol_mint:F mint_to:U/msol_ta msol_mint_auth:F clock:F rent:F system:F token:F stake_program:F",
        Gate::None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn withdraw_stake_account(
    a: &Inst,
    stake: &Pubkey,
    split: &Pubkey,
    rent_payer: &Pubkey,
    msol_from: &Pubkey,
    authority: &Pubkey,
    stake_index: u32,
    validator_index: u32,
    msol_amount: u64,
    beneficiary: &Pubkey,
) -> Ix {
    mk(
        "withdraw_stake_account",
        accounts::WithdrawStakeAccount {
            state: a.state,
            msol_mint: a.msol_mint,
            burn_msol_from: *msol_from,
            burn_msol_authority: *authority,
            treasury_msol_account: a.treasury_msol,
            validator_list: a.validator_list,
            stake_list: a.stake_list,
            stake_withdraw_authority: a.withdraw_auth,
            stake_deposit_authority: a.deposit_auth,
            stake_account: *stake,
            split_stake_account: *split,
            split_stake_rent_payer: *rent_payer,
            clock: clock_id(),
            system_program: sys(),
            token_program: token_id(),
            stake_program: stake_id(),
        }
        .to_account_metas(None),
        instruction::WithdrawStakeAccount { stake_index, validator_index, msol_amount, beneficiary: *beneficiary }.data(),
        "state:F msol_mint:F burn_from:U/msol_ta authority:U/wallet treasury_msol:F validator_list:F stake_list:F withdraw_auth:F deposit_auth:F stake_account:F split:N rent_payer:U/wallet clock:F system:F token:F stake_program:F",
        Gate::None,
    )
}

pub fn realloc_validator_list(a: &Inst, rent_funds: &Pubkey, capacity: u32) -> Ix {
    mk(
        "realloc_validator_list",
        accounts::ReallocValidatorList { state: a.state, admin_authority: a.admin, validator_list: a.validator_list, rent_funds: *rent_funds, system_program: sys() }
            .to_account_metas(None),
        instruction::ReallocValidatorList { capacity }.data(),
        "state:F admin:A validator_list:F rent_funds:U/wallet system:F",
        Gate::Admin,
    )
}

pub fn realloc_stake_list(a: &Inst, rent_funds: &Pubkey, capacity: u32) -> Ix {
    mk(
        "realloc_stake_list",
        accounts::ReallocStakeList { state: a.state, admin_authority: a.admin, stake_list: a.stake_list, rent_funds: *rent_funds, system_program: sys() }
            .to_account_metas(None),
        instruction::ReallocStakeList { capacity }.data(),
        "state:F admin:A stake_list:F rent_funds:U/wallet system:F",
        Gate::Admin,
    )
}

pub fn finalize_delinquent_upgrade(a: &Inst, max_validators: u32) -> Ix {
    mk(
        "finalize_delinquent_upgrade",
        accounts::FinalizeDelinquentUpgrade { state: a.state, validator_list: a.validator_list }.to_account_metas(None),
        instruction::FinalizeDelinquentUpgrade { max_validators }.data(),
        "state:F validator_list:F",
        Gate::None,
    )
}

#[allow(dead_code)]
pub fn user_dummy(_u: &User) {}

pub const ALL_IX: &[&str] = &[
    "initialize",
    "change_authority",
    "add_validator",
    "remove_validator",
    "set_validator_score",
    "config_validator_system",
    "deposit",
    "deposit_stake_account",
    "liquid_unstake",
    "add_liquidity",
    "remove_liquidity",
    "config_lp",
    "config_marinade",
    "order_unstake",
    "claim",
    "stake_reserve",
    "update_active",
    "update_deactivated",
    "deactivate_stake",
    "emergency_unstake",
    "partial_unstake",
    "merge_stakes",
    "create_canonical_stake",
    "pause",
    "resume",
    "withdraw_stake_account",
    "realloc_validator_list",
    "realloc_stake_list",
    "finalize_delinquent_upgrade",
];
