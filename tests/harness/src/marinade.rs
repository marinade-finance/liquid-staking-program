//! Marinade fixture, instruction builders (`ix::*`), typed ops (`Marinade::*`) and readers.

use crate::convert::{marinade_ix, to_addr, to_anchor};
use crate::env::{sol, TestEnv, TxFailure, TxMeta, TxResult};
use crate::profile::addr;
use anchor_lang::{solana_program, solana_program::program_pack::Pack, AccountDeserialize, AnchorDeserialize};
use anyhow::{anyhow, Result};
use marinade_finance::instructions::admin::{ConfigLpParams, ConfigMarinadeParams, InitializeData, LiqPoolInitializeData};
use marinade_finance::state::delayed_unstake_ticket::TicketAccountData;
use marinade_finance::state::fee::{Fee, FeeCents};
use marinade_finance::state::stake_system::{StakeRecord, StakeSystem};
use marinade_finance::state::validator_system::{ValidatorRecord, ValidatorSystem};
use marinade_finance::state::liq_pool::LiqPool;
use marinade_finance::State;
use solana_account::Account;
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

pub const PROGRAM_ID_STR: &str = "MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD";

pub fn program_id() -> Address {
    addr(PROGRAM_ID_STR)
}

fn a(x: &Address) -> anchor_lang::prelude::Pubkey {
    to_anchor(x)
}

mod ids {
    use super::*;
    pub fn system() -> anchor_lang::prelude::Pubkey {
        solana_program::system_program::ID
    }
    pub fn token() -> anchor_lang::prelude::Pubkey {
        anchor_spl::token::ID
    }
    pub fn stake() -> anchor_lang::prelude::Pubkey {
        solana_program::stake::program::ID
    }
    pub fn clock() -> anchor_lang::prelude::Pubkey {
        solana_program::sysvar::clock::ID
    }
    pub fn rent() -> anchor_lang::prelude::Pubkey {
        solana_program::sysvar::rent::ID
    }
    pub fn epoch_schedule() -> anchor_lang::prelude::Pubkey {
        solana_program::sysvar::epoch_schedule::ID
    }
    pub fn stake_history() -> anchor_lang::prelude::Pubkey {
        solana_program::sysvar::stake_history::ID
    }
    pub fn stake_config() -> anchor_lang::prelude::Pubkey {
        solana_program::stake::config::ID
    }
}

/// Addresses of one Marinade instance (+ the authority keys).
pub struct Marinade {
    pub state: Address,
    pub admin: Keypair,
    pub validator_manager: Keypair,
    pub pause_authority: Keypair,
    /// operational SOL account (receives stake rent back), a system account
    pub operational: Address,
    pub treasury_owner: Address,
    pub msol_mint: Address,
    pub lp_mint: Address,
    pub msol_leg: Address,
    pub treasury_msol: Address,
    pub reserve: Address,
    pub sol_leg: Address,
    pub stake_list: Address,
    pub validator_list: Address,
    pub deposit_auth: Address,
    pub withdraw_auth: Address,
    pub msol_mint_auth: Address,
    pub lp_mint_auth: Address,
    pub msol_leg_auth: Address,
    pub slots_for_stake_delta: u64,
    /// dedicated crank payer for the operational stake rent top-up (`HARNESS_RENT_TOPUP=1`, see `topup.rs`)
    pub rent_payer: Keypair,
}

pub struct User {
    pub key: Keypair,
    pub msol: Address,
    pub lp: Address,
}

impl Clone for User {
    fn clone(&self) -> Self {
        User { key: crate::env::clone_keypair(&self.key), msol: self.msol, lp: self.lp }
    }
}

impl Clone for Marinade {
    fn clone(&self) -> Self {
        use crate::env::clone_keypair as ck;
        Marinade {
            state: self.state,
            admin: ck(&self.admin),
            validator_manager: ck(&self.validator_manager),
            pause_authority: ck(&self.pause_authority),
            operational: self.operational,
            treasury_owner: self.treasury_owner,
            msol_mint: self.msol_mint,
            lp_mint: self.lp_mint,
            msol_leg: self.msol_leg,
            treasury_msol: self.treasury_msol,
            reserve: self.reserve,
            sol_leg: self.sol_leg,
            stake_list: self.stake_list,
            validator_list: self.validator_list,
            deposit_auth: self.deposit_auth,
            withdraw_auth: self.withdraw_auth,
            msol_mint_auth: self.msol_mint_auth,
            lp_mint_auth: self.lp_mint_auth,
            msol_leg_auth: self.msol_leg_auth,
            slots_for_stake_delta: self.slots_for_stake_delta,
            rent_payer: ck(&self.rent_payer),
        }
    }
}

impl User {
    pub fn pubkey(&self) -> Address {
        self.key.pubkey()
    }
}

#[derive(Clone, Debug)]
pub struct SetupOpts {
    pub min_stake: u64,
    pub reward_fee_bp: u32,
    pub stake_capacity: u32,
    pub validator_capacity: u32,
    pub slots_for_stake_delta: u64,
    pub lp_target: u64,
    /// run config_marinade with the "all paths reachable" settings
    pub configure: bool,
}

impl Default for SetupOpts {
    fn default() -> Self {
        Self {
            min_stake: sol(1),
            reward_fee_bp: 300,
            stake_capacity: 128,
            validator_capacity: 32,
            slots_for_stake_delta: 3_000,
            lp_target: sol(100),
            configure: true,
        }
    }
}

/// A stake-moving op may leave a new (split / fresh) stake account behind.
#[derive(Debug)]
pub struct StakeOp {
    pub meta: TxMeta,
    pub new_stake: Option<Address>,
}

#[derive(Debug, Clone)]
pub struct IndexedStake {
    pub index: u32,
    pub record: StakeRecord,
}

/// Instruction builders (no execution) for custom transactions.
pub mod ix {
    use super::*;
    use marinade_finance::{accounts, instruction};

    pub fn deposit(m: &Marinade, from: &Address, mint_to: &Address, lamports: u64) -> Instruction {
        marinade_ix(
            accounts::Deposit {
                state: a(&m.state),
                msol_mint: a(&m.msol_mint),
                liq_pool_sol_leg_pda: a(&m.sol_leg),
                liq_pool_msol_leg: a(&m.msol_leg),
                liq_pool_msol_leg_authority: a(&m.msol_leg_auth),
                reserve_pda: a(&m.reserve),
                transfer_from: a(from),
                mint_to: a(mint_to),
                msol_mint_authority: a(&m.msol_mint_auth),
                system_program: ids::system(),
                token_program: ids::token(),
            },
            instruction::Deposit { lamports },
        )
    }

    pub fn liquid_unstake(m: &Marinade, msol_from: &Address, authority: &Address, sol_to: &Address, msol_amount: u64) -> Instruction {
        marinade_ix(
            accounts::LiquidUnstake {
                state: a(&m.state),
                msol_mint: a(&m.msol_mint),
                liq_pool_sol_leg_pda: a(&m.sol_leg),
                liq_pool_msol_leg: a(&m.msol_leg),
                treasury_msol_account: a(&m.treasury_msol),
                get_msol_from: a(msol_from),
                get_msol_from_authority: a(authority),
                transfer_sol_to: a(sol_to),
                system_program: ids::system(),
                token_program: ids::token(),
            },
            instruction::LiquidUnstake { msol_amount },
        )
    }

    pub fn add_liquidity(m: &Marinade, from: &Address, lp_to: &Address, lamports: u64) -> Instruction {
        marinade_ix(
            accounts::AddLiquidity {
                state: a(&m.state),
                lp_mint: a(&m.lp_mint),
                lp_mint_authority: a(&m.lp_mint_auth),
                liq_pool_msol_leg: a(&m.msol_leg),
                liq_pool_sol_leg_pda: a(&m.sol_leg),
                transfer_from: a(from),
                mint_to: a(lp_to),
                system_program: ids::system(),
                token_program: ids::token(),
            },
            instruction::AddLiquidity { lamports },
        )
    }

    pub fn remove_liquidity(m: &Marinade, lp_from: &Address, authority: &Address, sol_to: &Address, msol_to: &Address, tokens: u64) -> Instruction {
        marinade_ix(
            accounts::RemoveLiquidity {
                state: a(&m.state),
                lp_mint: a(&m.lp_mint),
                burn_from: a(lp_from),
                burn_from_authority: a(authority),
                transfer_sol_to: a(sol_to),
                transfer_msol_to: a(msol_to),
                liq_pool_sol_leg_pda: a(&m.sol_leg),
                liq_pool_msol_leg: a(&m.msol_leg),
                liq_pool_msol_leg_authority: a(&m.msol_leg_auth),
                system_program: ids::system(),
                token_program: ids::token(),
            },
            instruction::RemoveLiquidity { tokens },
        )
    }

    pub fn order_unstake(m: &Marinade, msol_from: &Address, authority: &Address, ticket: &Address, msol_amount: u64) -> Instruction {
        marinade_ix(
            accounts::OrderUnstake {
                state: a(&m.state),
                msol_mint: a(&m.msol_mint),
                burn_msol_from: a(msol_from),
                burn_msol_authority: a(authority),
                new_ticket_account: a(ticket),
                clock: ids::clock(),
                rent: ids::rent(),
                token_program: ids::token(),
            },
            instruction::OrderUnstake { msol_amount },
        )
    }

    pub fn claim(m: &Marinade, ticket: &Address, beneficiary: &Address) -> Instruction {
        marinade_ix(
            accounts::Claim {
                state: a(&m.state),
                reserve_pda: a(&m.reserve),
                ticket_account: a(ticket),
                transfer_sol_to: a(beneficiary),
                clock: ids::clock(),
                system_program: ids::system(),
            },
            instruction::Claim {},
        )
    }

    pub fn stake_reserve(m: &Marinade, validator_index: u32, vote: &Address, new_stake: &Address, rent_payer: &Address) -> Instruction {
        marinade_ix(
            accounts::StakeReserve {
                state: a(&m.state),
                validator_list: a(&m.validator_list),
                stake_list: a(&m.stake_list),
                validator_vote: a(vote),
                reserve_pda: a(&m.reserve),
                stake_account: a(new_stake),
                stake_deposit_authority: a(&m.deposit_auth),
                rent_payer: a(rent_payer),
                clock: ids::clock(),
                epoch_schedule: ids::epoch_schedule(),
                rent: ids::rent(),
                stake_history: ids::stake_history(),
                stake_config: ids::stake_config(),
                system_program: ids::system(),
                stake_program: ids::stake(),
            },
            instruction::StakeReserve { validator_index },
        )
    }

    fn update_common(m: &Marinade, stake: &Address) -> accounts::UpdateCommon {
        accounts::UpdateCommon {
            state: a(&m.state),
            stake_list: a(&m.stake_list),
            stake_account: a(stake),
            stake_withdraw_authority: a(&m.withdraw_auth),
            reserve_pda: a(&m.reserve),
            msol_mint: a(&m.msol_mint),
            msol_mint_authority: a(&m.msol_mint_auth),
            treasury_msol_account: a(&m.treasury_msol),
            clock: ids::clock(),
            stake_history: ids::stake_history(),
            stake_program: ids::stake(),
            token_program: ids::token(),
            validator_list: a(&m.validator_list),
        }
    }

    pub fn update_active(m: &Marinade, stake: &Address, stake_index: u32, validator_index: u32) -> Instruction {
        marinade_ix(
            accounts::UpdateActive { common: update_common(m, stake) },
            instruction::UpdateActive { stake_index, validator_index },
        )
    }

    pub fn update_deactivated(m: &Marinade, stake: &Address, stake_index: u32, validator_index: u32) -> Instruction {
        marinade_ix(
            accounts::UpdateDeactivated {
                common: update_common(m, stake),
                operational_sol_account: a(&m.operational),
                system_program: ids::system(),
            },
            instruction::UpdateDeactivated { stake_index, validator_index },
        )
    }

    pub fn deactivate_stake(m: &Marinade, stake: &Address, split: &Address, rent_payer: &Address, stake_index: u32, validator_index: u32) -> Instruction {
        marinade_ix(
            accounts::DeactivateStake {
                state: a(&m.state),
                reserve_pda: a(&m.reserve),
                validator_list: a(&m.validator_list),
                stake_list: a(&m.stake_list),
                stake_account: a(stake),
                stake_deposit_authority: a(&m.deposit_auth),
                split_stake_account: a(split),
                split_stake_rent_payer: a(rent_payer),
                clock: ids::clock(),
                rent: ids::rent(),
                epoch_schedule: ids::epoch_schedule(),
                stake_history: ids::stake_history(),
                system_program: ids::system(),
                stake_program: ids::stake(),
            },
            instruction::DeactivateStake { stake_index, validator_index },
        )
    }

    pub fn merge_stakes(m: &Marinade, dest: &Address, source: &Address, dest_index: u32, source_index: u32, validator_index: u32) -> Instruction {
        marinade_ix(
            accounts::MergeStakes {
                state: a(&m.state),
                stake_list: a(&m.stake_list),
                validator_list: a(&m.validator_list),
                destination_stake: a(dest),
                source_stake: a(source),
                stake_deposit_authority: a(&m.deposit_auth),
                stake_withdraw_authority: a(&m.withdraw_auth),
                operational_sol_account: a(&m.operational),
                clock: ids::clock(),
                stake_history: ids::stake_history(),
                stake_program: ids::stake(),
            },
            instruction::MergeStakes {
                destination_stake_index: dest_index,
                source_stake_index: source_index,
                validator_index,
            },
        )
    }

    pub fn create_canonical_stake(m: &Marinade, canonical: &Address, source: &Address, source_index: u32, validator_index: u32) -> Instruction {
        marinade_ix(
            accounts::CreateCanonicalStake {
                state: a(&m.state),
                stake_list: a(&m.stake_list),
                validator_list: a(&m.validator_list),
                canonical_stake: a(canonical),
                source_stake: a(source),
                stake_deposit_authority: a(&m.deposit_auth),
                stake_withdraw_authority: a(&m.withdraw_auth),
                operational_sol_account: a(&m.operational),
                clock: ids::clock(),
                stake_history: ids::stake_history(),
                stake_program: ids::stake(),
                system_program: ids::system(),
            },
            instruction::CreateCanonicalStake { source_stake_index: source_index, validator_index },
        )
    }

    pub fn partial_unstake(m: &Marinade, stake: &Address, split: &Address, rent_payer: &Address, stake_index: u32, validator_index: u32, desired: u64) -> Instruction {
        marinade_ix(
            accounts::PartialUnstake {
                state: a(&m.state),
                validator_manager_authority: a(&m.validator_manager.pubkey()),
                validator_list: a(&m.validator_list),
                stake_list: a(&m.stake_list),
                stake_account: a(stake),
                stake_deposit_authority: a(&m.deposit_auth),
                reserve_pda: a(&m.reserve),
                split_stake_account: a(split),
                split_stake_rent_payer: a(rent_payer),
                clock: ids::clock(),
                rent: ids::rent(),
                stake_history: ids::stake_history(),
                system_program: ids::system(),
                stake_program: ids::stake(),
            },
            instruction::PartialUnstake { stake_index, validator_index, desired_unstake_amount: desired },
        )
    }

    pub fn emergency_unstake(m: &Marinade, stake: &Address, stake_index: u32, validator_index: u32) -> Instruction {
        marinade_ix(
            accounts::EmergencyUnstake {
                state: a(&m.state),
                validator_manager_authority: a(&m.validator_manager.pubkey()),
                validator_list: a(&m.validator_list),
                stake_list: a(&m.stake_list),
                stake_account: a(stake),
                stake_deposit_authority: a(&m.deposit_auth),
                clock: ids::clock(),
                stake_program: ids::stake(),
            },
            instruction::EmergencyUnstake { stake_index, validator_index },
        )
    }

    pub fn add_validator(m: &Marinade, vote: &Address, rent_payer: &Address, score: u32) -> Instruction {
        marinade_ix(
            accounts::AddValidator {
                state: a(&m.state),
                manager_authority: a(&m.validator_manager.pubkey()),
                validator_list: a(&m.validator_list),
                validator_vote: a(vote),
                duplication_flag: a(&m.duplication_flag(vote)),
                rent_payer: a(rent_payer),
                clock: ids::clock(),
                rent: ids::rent(),
                system_program: ids::system(),
            },
            instruction::AddValidator { score },
        )
    }

    pub fn remove_validator(m: &Marinade, index: u32, vote: &Address) -> Instruction {
        marinade_ix(
            accounts::RemoveValidator {
                state: a(&m.state),
                manager_authority: a(&m.validator_manager.pubkey()),
                validator_list: a(&m.validator_list),
                duplication_flag: a(&m.duplication_flag(vote)),
                operational_sol_account: a(&m.operational),
            },
            instruction::RemoveValidator { index, validator_vote: a(vote) },
        )
    }

    pub fn set_validator_score(m: &Marinade, index: u32, vote: &Address, score: u32) -> Instruction {
        marinade_ix(
            accounts::SetValidatorScore {
                state: a(&m.state),
                manager_authority: a(&m.validator_manager.pubkey()),
                validator_list: a(&m.validator_list),
            },
            instruction::SetValidatorScore { index, validator_vote: a(vote), score },
        )
    }

    pub fn config_validator_system(m: &Marinade, extra_runs: u32) -> Instruction {
        marinade_ix(
            accounts::ConfigValidatorSystem { state: a(&m.state), manager_authority: a(&m.validator_manager.pubkey()) },
            instruction::ConfigValidatorSystem { extra_runs },
        )
    }

    pub fn config_marinade(m: &Marinade, params: ConfigMarinadeParams) -> Instruction {
        marinade_ix(
            accounts::ConfigMarinade { state: a(&m.state), admin_authority: a(&m.admin.pubkey()) },
            instruction::ConfigMarinade { params },
        )
    }

    pub fn config_lp(m: &Marinade, params: ConfigLpParams) -> Instruction {
        marinade_ix(
            accounts::ConfigLp { state: a(&m.state), admin_authority: a(&m.admin.pubkey()) },
            instruction::ConfigLp { params },
        )
    }

    pub fn pause(m: &Marinade) -> Instruction {
        marinade_ix(
            accounts::EmergencyPause { state: a(&m.state), pause_authority: a(&m.pause_authority.pubkey()) },
            instruction::Pause {},
        )
    }

    pub fn resume(m: &Marinade) -> Instruction {
        marinade_ix(
            accounts::EmergencyPause { state: a(&m.state), pause_authority: a(&m.pause_authority.pubkey()) },
            instruction::Resume {},
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn deposit_stake_account(
        m: &Marinade,
        stake: &Address,
        stake_authority: &Address,
        vote: &Address,
        rent_payer: &Address,
        mint_to: &Address,
        validator_index: u32,
    ) -> Instruction {
        marinade_ix(
            accounts::DepositStakeAccount {
                state: a(&m.state),
                validator_list: a(&m.validator_list),
                stake_list: a(&m.stake_list),
                stake_account: a(stake),
                stake_authority: a(stake_authority),
                duplication_flag: a(&m.duplication_flag(vote)),
                rent_payer: a(rent_payer),
                msol_mint: a(&m.msol_mint),
                mint_to: a(mint_to),
                msol_mint_authority: a(&m.msol_mint_auth),
                clock: ids::clock(),
                rent: ids::rent(),
                system_program: ids::system(),
                token_program: ids::token(),
                stake_program: ids::stake(),
            },
            instruction::DepositStakeAccount { validator_index },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn withdraw_stake_account(
        m: &Marinade,
        stake: &Address,
        split: &Address,
        rent_payer: &Address,
        msol_from: &Address,
        authority: &Address,
        stake_index: u32,
        validator_index: u32,
        msol_amount: u64,
        beneficiary: &Address,
    ) -> Instruction {
        marinade_ix(
            accounts::WithdrawStakeAccount {
                state: a(&m.state),
                msol_mint: a(&m.msol_mint),
                burn_msol_from: a(msol_from),
                burn_msol_authority: a(authority),
                treasury_msol_account: a(&m.treasury_msol),
                validator_list: a(&m.validator_list),
                stake_list: a(&m.stake_list),
                stake_withdraw_authority: a(&m.withdraw_auth),
                stake_deposit_authority: a(&m.deposit_auth),
                stake_account: a(stake),
                split_stake_account: a(split),
                split_stake_rent_payer: a(rent_payer),
                clock: ids::clock(),
                system_program: ids::system(),
                token_program: ids::token(),
                stake_program: ids::stake(),
            },
            instruction::WithdrawStakeAccount { stake_index, validator_index, msol_amount, beneficiary: a(beneficiary) },
        )
    }

    pub fn create_ticket_account(m: &Marinade, from: &Address, ticket: &Address, lamports: u64) -> Instruction {
        let _ = m;
        let i = solana_system_interface::instruction::create_account(from, ticket, lamports, TICKET_SIZE as u64, &program_id());
        crate::convert_ix!(&i)
    }
}

pub const TICKET_SIZE: usize = 8 + 32 + 32 + 8 + 8;

impl Marinade {
    pub fn duplication_flag(&self, vote: &Address) -> Address {
        to_addr(&ValidatorRecord::find_duplication_flag(&a(&self.state), &a(vote)).0)
    }

    pub fn canonical_stake(&self, vote: &Address) -> Address {
        to_addr(&State::find_canonical_stake_address(&a(&self.state), &a(vote)).0)
    }

    /// Create a fresh Marinade instance exactly as `initialize` requires, then configure it.
    pub fn setup(env: &mut TestEnv, opts: &SetupOpts) -> Result<Marinade> {
        use anchor_spl::token::spl_token::state::Account as TA;
        let pid = program_id();
        let state = Address::new_unique();
        let sa = a(&state);
        let reserve = to_addr(&State::find_reserve_address(&sa).0);
        let msol_mint_auth = to_addr(&State::find_msol_mint_authority(&sa).0);
        let lp_mint_auth = to_addr(&LiqPool::find_lp_mint_authority(&sa).0);
        let sol_leg = to_addr(&LiqPool::find_sol_leg_address(&sa).0);
        let msol_leg_auth = to_addr(&LiqPool::find_msol_leg_authority(&sa).0);
        let deposit_auth = to_addr(&StakeSystem::find_stake_deposit_authority(&sa).0);
        let withdraw_auth = to_addr(&StakeSystem::find_stake_withdraw_authority(&sa).0);

        let msol_mint = env.create_mint(&msol_mint_auth, 9);
        let lp_mint = env.create_mint(&lp_mint_auth, 9);
        let treasury_owner = Address::new_unique();
        let treasury_msol = env.create_token_account(&msol_mint, &treasury_owner);
        let msol_leg = env.create_token_account(&msol_mint, &msol_leg_auth);

        let token_rent = env.rent_exempt(TA::LEN);
        for pda in [reserve, sol_leg] {
            env.set_account(
                pda,
                Account { lamports: token_rent, data: vec![], owner: solana_sdk_ids::system_program::ID, executable: false, rent_epoch: u64::MAX },
            );
        }
        let state_len = State::serialized_len();
        let program_owned = |env: &TestEnv, len: usize| Account {
            lamports: env.rent_exempt(len),
            data: vec![0u8; len],
            owner: pid,
            executable: false,
            rent_epoch: u64::MAX,
        };
        let acc = program_owned(env, state_len);
        env.set_account(state, acc);
        let stake_list = Address::new_unique();
        let validator_list = Address::new_unique();
        let acc = program_owned(env, StakeSystem::bytes_for_list(opts.stake_capacity, 0) as usize);
        env.set_account(stake_list, acc);
        let acc = program_owned(env, ValidatorSystem::bytes_for_list(opts.validator_capacity, 0) as usize);
        env.set_account(validator_list, acc);

        let operational = Address::new_unique();
        env.fund(&operational, sol(10));

        let m = Marinade {
            state,
            admin: Keypair::new(),
            validator_manager: Keypair::new(),
            pause_authority: Keypair::new(),
            operational,
            treasury_owner,
            msol_mint,
            lp_mint,
            msol_leg,
            treasury_msol,
            reserve,
            sol_leg,
            stake_list,
            validator_list,
            deposit_auth,
            withdraw_auth,
            msol_mint_auth,
            lp_mint_auth,
            msol_leg_auth,
            slots_for_stake_delta: opts.slots_for_stake_delta,
            rent_payer: Keypair::new(),
        };
        // authorities pay no fees but sign; give them lamports anyway
        for k in [&m.admin, &m.validator_manager, &m.pause_authority] {
            env.fund(&k.pubkey(), sol(1));
        }

        let data = InitializeData {
            admin_authority: a(&m.admin.pubkey()),
            validator_manager_authority: a(&m.validator_manager.pubkey()),
            min_stake: opts.min_stake,
            rewards_fee: Fee::from_basis_points(opts.reward_fee_bp),
            liq_pool: LiqPoolInitializeData {
                lp_liquidity_target: opts.lp_target,
                lp_max_fee: Fee::from_basis_points(300),
                lp_min_fee: Fee::from_basis_points(30),
                lp_treasury_cut: Fee::from_basis_points(2500),
            },
            additional_stake_record_space: 0,
            additional_validator_record_space: 0,
            slots_for_stake_delta: opts.slots_for_stake_delta,
            pause_authority: a(&m.pause_authority.pubkey()),
        };
        let init = marinade_ix(
            marinade_finance::accounts::Initialize {
                state: a(&state),
                reserve_pda: a(&reserve),
                stake_list: a(&stake_list),
                validator_list: a(&validator_list),
                msol_mint: a(&msol_mint),
                operational_sol_account: a(&operational),
                liq_pool: marinade_finance::accounts::LiqPoolInitialize {
                    lp_mint: a(&lp_mint),
                    sol_leg_pda: a(&sol_leg),
                    msol_leg: a(&msol_leg),
                },
                treasury_msol_account: a(&treasury_msol),
                clock: ids::clock(),
                rent: ids::rent(),
            },
            marinade_finance::instruction::Initialize { data },
        );
        env.send(&[init], &[]).map_err(|e| anyhow!("initialize failed: {e}"))?;
        if opts.configure {
            m.configure_all_paths(env)?;
        }
        Ok(m)
    }

    /// config_marinade so every path (withdraw_stake_account, fees, min amounts) is reachable.
    pub fn configure_all_paths(&self, env: &mut TestEnv) -> Result<()> {
        let params = ConfigMarinadeParams {
            withdraw_stake_account_enabled: Some(true),
            delayed_unstake_fee: Some(FeeCents::from_bp_cents(1_000)),
            withdraw_stake_account_fee: Some(FeeCents::from_bp_cents(1_000)),
            deposit_sol_fee: Some(FeeCents::from_bp_cents(500)),
            deposit_stake_account_fee: Some(FeeCents::from_bp_cents(500)),
            min_deposit: Some(1_000_000),
            min_withdraw: Some(1_000_000),
            ..Default::default()
        };
        self.config_marinade(env, params).map_err(|e| anyhow!("config_marinade: {e}"))?;
        Ok(())
    }

    // ------------------------------------------------------------ readers

    pub fn state(&self, env: &TestEnv) -> State {
        let d = env.data(&self.state).expect("state account");
        State::try_deserialize(&mut &d[..]).expect("state deserialize")
    }

    fn list<T: AnchorDeserialize>(&self, env: &TestEnv, acct: &Address, count: u32, item_size: u32) -> Vec<T> {
        let d = env.data(acct).expect("list account");
        (0..count)
            .map(|i| {
                let s = 8 + (i * item_size) as usize;
                T::deserialize(&mut &d[s..s + item_size as usize]).expect("list item")
            })
            .collect()
    }

    pub fn stake_records(&self, env: &TestEnv) -> Vec<StakeRecord> {
        let st = self.state(env);
        self.list(env, &self.stake_list, st.stake_system.stake_count(), st.stake_system.stake_record_size())
    }

    pub fn indexed_stakes(&self, env: &TestEnv) -> Vec<IndexedStake> {
        self.stake_records(env).into_iter().enumerate().map(|(i, r)| IndexedStake { index: i as u32, record: r }).collect()
    }

    pub fn validator_records(&self, env: &TestEnv) -> Vec<ValidatorRecord> {
        let st = self.state(env);
        self.list(env, &self.validator_list, st.validator_system.validator_count(), st.validator_system.validator_record_size())
    }

    pub fn validator_index(&self, env: &TestEnv, vote: &Address) -> Option<u32> {
        self.validator_records(env).iter().position(|v| to_addr(&v.validator_account) == *vote).map(|i| i as u32)
    }

    pub fn stake_index(&self, env: &TestEnv, stake: &Address) -> Option<u32> {
        self.stake_records(env).iter().position(|r| to_addr(&r.stake_account) == *stake).map(|i| i as u32)
    }

    pub fn tickets(&self, env: &TestEnv) -> Vec<(Address, TicketAccountData)> {
        env.accounts_owned_by(&program_id())
            .into_iter()
            .filter(|(_, acc)| acc.data.len() == TICKET_SIZE)
            .filter_map(|(k, acc)| TicketAccountData::try_deserialize(&mut &acc.data[..]).ok().map(|t| (k, t)))
            .filter(|(_, t)| to_addr(&t.state_address) == self.state)
            .collect()
    }

    pub fn ticket(&self, env: &TestEnv, ticket: &Address) -> Option<TicketAccountData> {
        let d = env.data(ticket)?;
        TicketAccountData::try_deserialize(&mut &d[..]).ok()
    }

    /// (total_virtual_staked_lamports, msol_supply)
    pub fn price_parts(&self, env: &TestEnv) -> (u64, u64) {
        let s = self.state(env);
        (s.total_virtual_staked_lamports(), s.msol_supply)
    }

    pub fn msol_price(&self, env: &TestEnv) -> f64 {
        let (t, s) = self.price_parts(env);
        if s == 0 { 1.0 } else { t as f64 / s as f64 }
    }

    // ------------------------------------------------------------ users

    pub fn new_user(&self, env: &mut TestEnv, lamports: u64) -> User {
        let key = env.new_funded(lamports);
        let msol = env.create_token_account(&self.msol_mint, &key.pubkey());
        let lp = env.create_token_account(&self.lp_mint, &key.pubkey());
        User { key, msol, lp }
    }

    // ------------------------------------------------------------ ops: user

    pub fn deposit(&self, env: &mut TestEnv, user: &User, lamports: u64) -> TxResult {
        env.send(&[ix::deposit(self, &user.pubkey(), &user.msol, lamports)], &[&user.key])
    }

    pub fn liquid_unstake(&self, env: &mut TestEnv, user: &User, msol_amount: u64) -> TxResult {
        env.send(&[ix::liquid_unstake(self, &user.msol, &user.pubkey(), &user.pubkey(), msol_amount)], &[&user.key])
    }

    pub fn add_liquidity(&self, env: &mut TestEnv, user: &User, lamports: u64) -> TxResult {
        env.send(&[ix::add_liquidity(self, &user.pubkey(), &user.lp, lamports)], &[&user.key])
    }

    pub fn remove_liquidity(&self, env: &mut TestEnv, user: &User, tokens: u64) -> TxResult {
        env.send(
            &[ix::remove_liquidity(self, &user.lp, &user.pubkey(), &user.pubkey(), &user.msol, tokens)],
            &[&user.key],
        )
    }

    /// Create the ticket account and order the unstake in one tx; returns the ticket address.
    pub fn order_unstake(&self, env: &mut TestEnv, user: &User, msol_amount: u64) -> Result<Address, TxFailure> {
        let ticket = Keypair::new();
        let lamports = env.rent_exempt(TICKET_SIZE);
        let ixs = [
            ix::create_ticket_account(self, &env.payer.pubkey(), &ticket.pubkey(), lamports),
            ix::order_unstake(self, &user.msol, &user.pubkey(), &ticket.pubkey(), msol_amount),
        ];
        env.send(&ixs, &[&ticket, &user.key])?;
        Ok(ticket.pubkey())
    }

    pub fn claim(&self, env: &mut TestEnv, ticket: &Address, beneficiary: &Address) -> TxResult {
        env.send(&[ix::claim(self, ticket, beneficiary)], &[])
    }

    /// Create a delegated user stake (through the stake program) and deposit it.
    pub fn deposit_stake_account(&self, env: &mut TestEnv, user: &User, stake: &Address, validator_index: u32) -> TxResult {
        let vote = to_addr(&self.validator_records(env)[validator_index as usize].validator_account);
        let payer = env.payer.pubkey();
        env.send(
            &[ix::deposit_stake_account(self, stake, &user.pubkey(), &vote, &payer, &user.msol, validator_index)],
            &[&user.key],
        )
    }

    pub fn withdraw_stake_account(&self, env: &mut TestEnv, user: &User, stake_index: u32, validator_index: u32, msol_amount: u64, beneficiary: &Address) -> Result<StakeOp, TxFailure> {
        let split = Keypair::new();
        let payer = env.payer.pubkey();
        let stake = to_addr(&self.stake_records(env)[stake_index as usize].stake_account);
        let meta = env.send(
            &[ix::withdraw_stake_account(self, &stake, &split.pubkey(), &payer, &user.msol, &user.pubkey(), stake_index, validator_index, msol_amount, beneficiary)],
            &[&split, &user.key],
        )?;
        Ok(StakeOp { meta, new_stake: Some(split.pubkey()) })
    }

    // ------------------------------------------------------------ ops: crank / management

    /// stake_reserve towards a validator; `new_stake` is Some only if a stake account was created.
    pub fn stake_reserve(&self, env: &mut TestEnv, validator_index: u32) -> Result<StakeOp, TxFailure> {
        let vote = to_addr(&self.validator_records(env)[validator_index as usize].validator_account);
        let before = self.state(env).stake_system.stake_count();
        let k = Keypair::new();
        let payer = env.payer.pubkey();
        let meta = env.send(&[ix::stake_reserve(self, validator_index, &vote, &k.pubkey(), &payer)], &[&k])?;
        let after = self.state(env).stake_system.stake_count();
        Ok(StakeOp { meta, new_stake: (after > before).then(|| k.pubkey()) })
    }

    pub fn update_active(&self, env: &mut TestEnv, stake_index: u32, validator_index: u32) -> TxResult {
        self.operational_top_up(env);
        let stake = to_addr(&self.stake_records(env)[stake_index as usize].stake_account);
        env.send(&[ix::update_active(self, &stake, stake_index, validator_index)], &[])
    }

    pub fn update_deactivated(&self, env: &mut TestEnv, stake_index: u32, validator_index: u32) -> TxResult {
        self.operational_top_up(env);
        let stake = to_addr(&self.stake_records(env)[stake_index as usize].stake_account);
        env.send(&[ix::update_deactivated(self, &stake, stake_index, validator_index)], &[])
    }

    pub fn deactivate_stake(&self, env: &mut TestEnv, stake_index: u32, validator_index: u32) -> Result<StakeOp, TxFailure> {
        self.operational_top_up(env);
        let stake = to_addr(&self.stake_records(env)[stake_index as usize].stake_account);
        let before = self.state(env).stake_system.stake_count();
        let k = Keypair::new();
        let payer = env.payer.pubkey();
        let meta = env.send(&[ix::deactivate_stake(self, &stake, &k.pubkey(), &payer, stake_index, validator_index)], &[&k])?;
        let after = self.state(env).stake_system.stake_count();
        Ok(StakeOp { meta, new_stake: (after > before).then(|| k.pubkey()) })
    }

    pub fn merge_stakes(&self, env: &mut TestEnv, dest_index: u32, source_index: u32, validator_index: u32) -> TxResult {
        self.operational_top_up(env);
        let recs = self.stake_records(env);
        let d = to_addr(&recs[dest_index as usize].stake_account);
        let s = to_addr(&recs[source_index as usize].stake_account);
        env.send(&[ix::merge_stakes(self, &d, &s, dest_index, source_index, validator_index)], &[])
    }

    pub fn create_canonical_stake(&self, env: &mut TestEnv, source_index: u32, validator_index: u32) -> TxResult {
        self.operational_top_up(env);
        let recs = self.stake_records(env);
        let vote = to_addr(&self.validator_records(env)[validator_index as usize].validator_account);
        let canonical = self.canonical_stake(&vote);
        let s = to_addr(&recs[source_index as usize].stake_account);
        env.send(&[ix::create_canonical_stake(self, &canonical, &s, source_index, validator_index)], &[])
    }

    pub fn partial_unstake(&self, env: &mut TestEnv, stake_index: u32, validator_index: u32, desired: u64) -> Result<StakeOp, TxFailure> {
        self.operational_top_up(env);
        let stake = to_addr(&self.stake_records(env)[stake_index as usize].stake_account);
        let before = self.state(env).stake_system.stake_count();
        let k = Keypair::new();
        let payer = env.payer.pubkey();
        let meta = env.send(
            &[ix::partial_unstake(self, &stake, &k.pubkey(), &payer, stake_index, validator_index, desired)],
            &[&k, &self.validator_manager],
        )?;
        let after = self.state(env).stake_system.stake_count();
        Ok(StakeOp { meta, new_stake: (after > before).then(|| k.pubkey()) })
    }

    pub fn emergency_unstake(&self, env: &mut TestEnv, stake_index: u32, validator_index: u32) -> TxResult {
        self.operational_top_up(env);
        let stake = to_addr(&self.stake_records(env)[stake_index as usize].stake_account);
        env.send(&[ix::emergency_unstake(self, &stake, stake_index, validator_index)], &[&self.validator_manager])
    }

    pub fn add_validator(&self, env: &mut TestEnv, vote: &Address, score: u32) -> Result<u32, TxFailure> {
        let payer = env.payer.pubkey();
        env.send(&[ix::add_validator(self, vote, &payer, score)], &[&self.validator_manager])?;
        Ok(self.state(env).validator_system.validator_count() - 1)
    }

    pub fn remove_validator(&self, env: &mut TestEnv, index: u32, vote: &Address) -> TxResult {
        env.send(&[ix::remove_validator(self, index, vote)], &[&self.validator_manager])
    }

    pub fn set_validator_score(&self, env: &mut TestEnv, index: u32, vote: &Address, score: u32) -> TxResult {
        env.send(&[ix::set_validator_score(self, index, vote, score)], &[&self.validator_manager])
    }

    pub fn config_validator_system(&self, env: &mut TestEnv, extra_runs: u32) -> TxResult {
        env.send(&[ix::config_validator_system(self, extra_runs)], &[&self.validator_manager])
    }

    pub fn config_marinade(&self, env: &mut TestEnv, params: ConfigMarinadeParams) -> TxResult {
        env.send(&[ix::config_marinade(self, params)], &[&self.admin])
    }

    pub fn config_lp(&self, env: &mut TestEnv, params: ConfigLpParams) -> TxResult {
        env.send(&[ix::config_lp(self, params)], &[&self.admin])
    }

    pub fn pause(&self, env: &mut TestEnv) -> TxResult {
        env.send(&[ix::pause(self)], &[&self.pause_authority])
    }

    pub fn resume(&self, env: &mut TestEnv) -> TxResult {
        env.send(&[ix::resume(self)], &[&self.pause_authority])
    }

    /// Add a validator with a fresh vote account; returns (vote, index).
    pub fn add_new_validator(&self, env: &mut TestEnv, score: u32) -> Result<(Address, u32)> {
        let vote = env.create_vote_account();
        let idx = self.add_validator(env, &vote, score).map_err(|e| anyhow!("add_validator: {e}"))?;
        Ok((vote, idx))
    }
}
