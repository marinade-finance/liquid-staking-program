use anchor_lang::prelude::*;

// NOTE: Anchor 0.27 adds 6000 for user error codes)
// (old Anchor 0.18 added 300 for user error codes)
#[error_code]
pub enum MarinadeError {
    #[msg("Wrong reserve owner. Must be a system account")]
    WrongReserveOwner, // 6000 0x1770

    #[msg("Reserve must have no data, but has data")]
    NonEmptyReserveData, // 6001 0x1771

    #[msg("Invalid initial reserve lamports")]
    InvalidInitialReserveLamports, // 6002 0x1772

    #[msg("Zero validator chunk size")]
    ZeroValidatorChunkSize, // 6003 0x1773

    #[msg("Too big validator chunk size")]
    TooBigValidatorChunkSize, // 6004 0x1774

    #[msg("Zero credit chunk size")]
    ZeroCreditChunkSize, // 6005 0x1775

    #[msg("Too big credit chunk size")]
    TooBigCreditChunkSize, // 6006 0x1776

    #[msg("Too low credit fee")]
    TooLowCreditFee, // 6007 0x1777

    #[msg("Invalid mint authority")]
    InvalidMintAuthority, // 6008 0x1778

    #[msg("Non empty initial mint supply")]
    MintHasInitialSupply, // 6009 0x1779

    #[msg("Invalid owner fee state")]
    InvalidOwnerFeeState, // 6010 0x177a

    #[msg(
        "Invalid program id. For using program from another account please update id in the code"
    )]
    InvalidProgramId, // 6011 0x177b

    #[msg("Unexpected account")]
    UnexpectedAccount, // 6012 0x177c

    #[msg("Calculation failure")]
    CalculationFailure, // 6013 0x177d

    #[msg("You can't deposit a stake-account with lockup")]
    StakeAccountWithLockup, // 6014 0x177e

    #[msg("Min stake is too low")]
    MinStakeIsTooLow, // 6015 0x177f

    #[msg("Lp max fee is too high")]
    LpMaxFeeIsTooHigh, // 6016 0x1780

    #[msg("Basis points overflow")]
    BasisPointsOverflow, // 6017 0x1781

    #[msg("LP min fee > LP max fee")]
    LpFeesAreWrongWayRound, // 6018 0x1782

    #[msg("Liquidity target too low")]
    LiquidityTargetTooLow, // 6019 0x1783

    #[msg("Ticket not due. Wait more epochs")]
    TicketNotDue, // 6020 0x1784

    #[msg("Ticket not ready. Wait a few hours and try again")]
    TicketNotReady, // 6021 0x1785

    #[msg("Wrong Ticket Beneficiary")]
    WrongBeneficiary, // 6022 0x1786

    #[msg("Stake Account not updated yet")]
    StakeAccountNotUpdatedYet, // 6023 0x1787

    #[msg("Stake Account not delegated")]
    StakeNotDelegated, // 6024 0x1788

    #[msg("Stake Account is emergency unstaking")]
    StakeAccountIsEmergencyUnstaking, // 6025 0x1789

    #[msg("Insufficient Liquidity in the Liquidity Pool")]
    InsufficientLiquidity, // 6026 0x178a

    #[msg("Auto adding a validator is not enabled")]
    AutoAddValidatorIsNotEnabled, // 6027 0x178b

    #[msg("Invalid admin authority")]
    InvalidAdminAuthority, // 6028 0x178c

    #[msg("Invalid validator system manager")]
    InvalidValidatorManager, // 6029 0x178d

    #[msg("Invalid stake list account discriminator")]
    InvalidStakeListDiscriminator, // 6030 0x178e

    #[msg("Invalid validator list account discriminator")]
    InvalidValidatorListDiscriminator, // 6031 0x178f

    #[msg("Treasury cut is too high")]
    TreasuryCutIsTooHigh, // 6032 0x1790

    #[msg("Reward fee is too high")]
    RewardsFeeIsTooHigh, // 6033 0x1791

    #[msg("Staking is capped")]
    StakingIsCapped, // 6034 0x1792

    #[msg("Liquidity is capped")]
    LiquidityIsCapped, // 6035 0x1793

    #[msg("Update window is too low")]
    UpdateWindowIsTooLow, // 6036 0x1794

    #[msg("Min withdraw is too high")]
    MinWithdrawIsTooHigh, // 6037 0x1795

    #[msg("Withdraw amount is too low")]
    WithdrawAmountIsTooLow, // 6038 0x1796

    #[msg("Deposit amount is too low")]
    DepositAmountIsTooLow, // 6039 0x1797

    #[msg("Not enough user funds")]
    NotEnoughUserFunds, // 6040 0x1798

    #[msg("Wrong token owner or delegate")]
    WrongTokenOwnerOrDelegate, // 6041 0x1799

    #[msg("Too early for stake delta")]
    TooEarlyForStakeDelta, // 6042 0x179a

    #[msg("Required delegated stake")]
    RequiredDelegatedStake, // 6043 0x179b

    #[msg("Required active stake")]
    RequiredActiveStake, // 6044 0x179c

    #[msg("Required deactivating stake")]
    RequiredDeactivatingStake, // 6045 0x179d

    #[msg("Depositing not activated stake")]
    DepositingNotActivatedStake, // 6046 0x179e

    #[msg("Too low delegation in the depositing stake")]
    TooLowDelegationInDepositingStake, // 6047 0x179f

    #[msg("Wrong deposited stake balance")]
    WrongStakeBalance, // 6048 0x17a0

    #[msg("Wrong validator account or index")]
    WrongValidatorAccountOrIndex, // 6049 0x17a1

    #[msg("Wrong stake account or index")]
    WrongStakeAccountOrIndex, // 6050 0x17a2

    #[msg("Delta stake is positive so we must stake instead of unstake")]
    UnstakingOnPositiveDelta, // 6051 0x17a3

    #[msg("Delta stake is negative so we must unstake instead of stake")]
    StakingOnNegativeDelta, // 6052 0x17a4

    // Not used
    #[msg("Invalid empty stake balance")]
    InvalidEmptyStakeBalance, // 6053 0x17a5

    #[msg("Stake must be uninitialized")]
    StakeMustBeUninitialized, // 6054 0x17a6

    // merge stakes
    #[msg("Destination stake must be delegated")]
    DestinationStakeMustBeDelegated, // 6055 0x17a7

    #[msg("Destination stake must not be deactivating")]
    DestinationStakeMustNotBeDeactivating, // 6056 0x17a8

    #[msg("Destination stake must be updated")]
    DestinationStakeMustBeUpdated, // 6057 0x17a9

    #[msg("Invalid destination stake delegation")]
    InvalidDestinationStakeDelegation, // 6058 0x17aa

    #[msg("Source stake must be delegated")]
    SourceStakeMustBeDelegated, // 6059 0x17ab

    #[msg("Source stake must not be deactivating")]
    SourceStakeMustNotBeDeactivating, // 6060 0x17ac

    #[msg("Source stake must be updated")]
    SourceStakeMustBeUpdated, // 6061 0x17ad

    #[msg("Invalid source stake delegation")]
    InvalidSourceStakeDelegation, // 6062 0x17ae

    #[msg("Invalid delayed unstake ticket")]
    InvalidDelayedUnstakeTicket, // 6063 0x17af

    #[msg("Reusing delayed unstake ticket")]
    ReusingDelayedUnstakeTicket, // 6064 0x17b0

    #[msg("Emergency unstaking from non zero scored validator")]
    EmergencyUnstakingFromNonZeroScoredValidator, // 6065 0x17b1

    #[msg("Wrong validator duplication flag")]
    WrongValidatorDuplicationFlag, // 6066 0x17b2

    #[msg("Redepositing marinade stake")]
    RedepositingMarinadeStake, // 6067 0x17b3

    #[msg("Removing validator with balance")]
    RemovingValidatorWithBalance, // 6068 0x17b4

    #[msg("Redelegate will put validator over stake target")]
    RedelegateOverTarget, // 6069 0x17b5

    #[msg("Source and Dest Validators are the same")]
    SourceAndDestValidatorsAreTheSame, // 6070 0x17b6

    #[msg("Some mSOL tokens was minted outside of marinade contract")]
    UnregisteredMsolMinted, // 6071 0x17b7

    #[msg("Some LP tokens was minted outside of marinade contract")]
    UnregisteredLPMinted, // 6072 0x17b8

    #[msg("List index out of bounds")]
    ListIndexOutOfBounds, // 6073 0x17b9

    #[msg("List overflow")]
    ListOverflow, // 6074 0x17ba

    #[msg("Requested pause and already Paused")]
    AlreadyPaused, // 6075 0x17bb

    #[msg("Requested resume, but not Paused")]
    NotPaused, // 6076 0x17bc

    #[msg("Emergency Pause is Active")]
    ProgramIsPaused, // 6077 0x17bd

    #[msg("Invalid pause authority")]
    InvalidPauseAuthority, // 6078 0x17be

    #[msg("Selected Stake account has not enough funds")]
    SelectedStakeAccountHasNotEnoughFunds, // 6079 0x17bf

    #[msg("Basis point CENTS overflow")]
    BasisPointCentsOverflow, // 6080 0x17c0

    #[msg("Withdraw stake account is not enabled")]
    WithdrawStakeAccountIsNotEnabled, // 6081 0x17c1

    #[msg("Withdraw stake account fee is too high")]
    WithdrawStakeAccountFeeIsTooHigh, // 6082 0x17c2

    #[msg("Delayed unstake fee is too high")]
    DelayedUnstakeFeeIsTooHigh, // 6083 0x17c3

    #[msg("Withdraw stake account value is too low")]
    WithdrawStakeLamportsIsTooLow, // 6084 0x17c4

    /// when the remainder after a withdraw stake account is less than min_stake
    #[msg("Stake account remainder too low")]
    StakeAccountRemainderTooLow, // 6085 0x17c5

    #[msg("Capacity of the list must be not less than it's current size")]
    ShrinkingListWithDeletingContents, // 6086 0x17c6
}