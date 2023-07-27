use anchor_lang::prelude::*;

// NOTE: Anchor 0.27 adds 6000 for user error codes)
// (old Anchor 0.18 added 300 for user error codes)
#[error_code]
pub enum MarinadeError {
    #[msg("Wrong reserve owner. Must be a system account")]
    WrongReserveOwner, // 6000

    #[msg("Reserve must have no data, but has data")]
    NonEmptyReserveData, // 6001

    #[msg("Invalid initial reserve lamports")]
    InvalidInitialReserveLamports, // 6002

    #[msg("Zero validator chunk size")]
    ZeroValidatorChunkSize, // 6003

    #[msg("Too big validator chunk size")]
    TooBigValidatorChunkSize, // 6004

    #[msg("Zero credit chunk size")]
    ZeroCreditChunkSize, // 6005

    #[msg("Too big credit chunk size")]
    TooBigCreditChunkSize, // 6006

    #[msg("Too low credit fee")]
    TooLowCreditFee, // 6007

    #[msg("Invalid mint authority")]
    InvalidMintAuthority, // 6008

    #[msg("Non empty initial mint supply")]
    MintHasInitialSupply, // 6009

    #[msg("Invalid owner fee state")]
    InvalidOwnerFeeState, // 6010

    #[msg(
        "Invalid program id. For using program from another account please update id in the code"
    )]
    InvalidProgramId, // 6011

    #[msg("Unexpected account")]
    UnexpectedAccount, // 6012

    #[msg("Calculation failure")]
    CalculationFailure, // 6013

    #[msg("You can't deposit a stake-account with lockup")]
    StakeAccountWithLockup, // 6014

    #[msg("Min stake is too low")]
    MinStakeIsTooLow, // 6015

    #[msg("Lp max fee is too high")]
    LpMaxFeeIsTooHigh, // 6016

    #[msg("Basis points overflow")]
    BasisPointsOverflow, // 6017

    #[msg("LP min fee > LP max fee")]
    LpFeesAreWrongWayRound, // 6018

    #[msg("Liquidity target too low")]
    LiquidityTargetTooLow, // 6019

    #[msg("Ticket not due. Wait more epochs")]
    TicketNotDue, // 6020

    #[msg("Ticket not ready. Wait a few hours and try again")]
    TicketNotReady, // 6021

    #[msg("Wrong Ticket Beneficiary")]
    WrongBeneficiary, // 6022

    #[msg("Stake Account not updated yet")]
    StakeAccountNotUpdatedYet, // 6023

    #[msg("Stake Account not delegated")]
    StakeNotDelegated, // 6024

    #[msg("Stake Account is emergency unstaking")]
    StakeAccountIsEmergencyUnstaking, // 6025

    #[msg("Insufficient Liquidity in the Liquidity Pool")]
    InsufficientLiquidity, // 6026

    #[msg("Auto adding a validator is not enabled")]
    AutoAddValidatorIsNotEnabled, // 6027

    #[msg("Invalid admin authority")]
    InvalidAdminAuthority, // 6028

    #[msg("Invalid validator system manager")]
    InvalidValidatorManager, // 6029

    #[msg("Invalid stake list account discriminator")]
    InvalidStakeListDiscriminator, // 6030

    #[msg("Invalid validator list account discriminator")]
    InvalidValidatorListDiscriminator, // 6031

    #[msg("Treasury cut is too high")]
    TreasuryCutIsTooHigh, // 6032

    #[msg("Reward fee is too high")]
    RewardsFeeIsTooHigh, // 6033

    #[msg("Staking is capped")]
    StakingIsCapped, // 6034

    #[msg("Liquidity is capped")]
    LiquidityIsCapped, // 6035

    #[msg("Update window is too low")]
    UpdateWindowIsTooLow, // 6036

    #[msg("Min withdraw is too high")]
    MinWithdrawIsTooHigh, // 6037

    #[msg("Withdraw amount is too low")]
    WithdrawAmountIsTooLow, // 6038

    #[msg("Deposit amount is too low")]
    DepositAmountIsTooLow, // 6039

    #[msg("Not enough user funds")]
    NotEnoughUserFunds, // 6040

    #[msg("Wrong token owner or delegate")]
    WrongTokenOwnerOrDelegate, // 6041

    #[msg("Too early for stake delta")]
    TooEarlyForStakeDelta, // 6042

    #[msg("Required delegated stake")]
    RequiredDelegatedStake, // 6043

    #[msg("Required active stake")]
    RequiredActiveStake, // 6044

    #[msg("Required deactivating stake")]
    RequiredDeactivatingStake, // 6045

    #[msg("Depositing not activated stake")]
    DepositingNotActivatedStake, // 6046

    #[msg("Too low delegation in the depositing stake")]
    TooLowDelegationInDepositingStake, // 6047

    #[msg("Wrong deposited stake balance")]
    WrongStakeBalance, // 6048

    #[msg("Wrong validator account or index")]
    WrongValidatorAccountOrIndex, // 6049

    #[msg("Wrong stake account or index")]
    WrongStakeAccountOrIndex, // 6050

    #[msg("Delta stake is positive so we must stake instead of unstake")]
    UnstakingOnPositiveDelta, // 6051

    #[msg("Delta stake is negative so we must unstake instead of stake")]
    StakingOnNegativeDelta, // 6052

    #[msg("Invalid empty stake balance")]
    InvalidEmptyStakeBalance, // 6053

    #[msg("Stake must be uninitialized")]
    StakeMustBeUninitialized, // 6054

    // merge stakes
    #[msg("Destination stake must be delegated")]
    DestinationStakeMustBeDelegated, // 6055

    #[msg("Destination stake must not be deactivating")]
    DestinationStakeMustNotBeDeactivating, // 6056

    #[msg("Destination stake must be updated")]
    DestinationStakeMustBeUpdated, // 6057

    #[msg("Invalid destination stake delegation")]
    InvalidDestinationStakeDelegation, // 6058

    #[msg("Source stake must be delegated")]
    SourceStakeMustBeDelegated, // 6059

    #[msg("Source stake must not be deactivating")]
    SourceStakeMustNotBeDeactivating, // 6060

    #[msg("Source stake must be updated")]
    SourceStakeMustBeUpdated, // 6061

    #[msg("Invalid source stake delegation")]
    InvalidSourceStakeDelegation, // 6062

    #[msg("Invalid delayed unstake ticket")]
    InvalidDelayedUnstakeTicket, // 6063

    #[msg("Reusing delayed unstake ticket")]
    ReusingDelayedUnstakeTicket, // 6064

    #[msg("Emergency unstaking from non zero scored validator")]
    EmergencyUnstakingFromNonZeroScoredValidator, // 6065

    #[msg("Wrong validator duplication flag")]
    WrongValidatorDuplicationFlag, // 6066

    #[msg("Redepositing marinade stake")]
    RedepositingMarinadeStake, // 6067

    #[msg("Removing validator with balance")]
    RemovingValidatorWithBalance, // 6068

    #[msg("Redelegate will put validator over stake target")]
    RedelegateOverTarget, // 6069

    #[msg("Source and Dest Validators are the same")]
    SourceAndDestValidatorsAreTheSame, // 6070

    #[msg("Some mSOL tokens was minted outside of marinade contract")]
    UnregisteredMsolMinted, // 6071

    #[msg("Some LP tokens was minted outside of marinade contract")]
    UnregisteredLPMinted, // 6072

    #[msg("List index out of bounds")]
    ListIndexOutOfBounds, // 6073

    #[msg("List overflow")]
    ListOverflow, // 6074

    #[msg("Requested pause and already Paused")]
    AlreadyPaused, // 6075

    #[msg("Requested resume, but not Paused")]
    NotPaused, // 6076

    #[msg("Emergency Pause is Active")]
    ProgramIsPaused, // 6077

    #[msg("Invalid pause authority")]
    InvalidPauseAuthority, // 6078

    #[msg("Selected Stake account has not enough funds")]
    SelectedStakeAccountHasNotEnoughFunds, // 6079

    #[msg("Basis point CENTS overflow")]
    BasisPointCentsOverflow, // 6080

    #[msg("Withdraw stake account is not enabled")]
    WithdrawStakeAccountIsNotEnabled, // 6081

    #[msg("Withdraw stake account fee is too high")]
    WithdrawStakeAccountFeeIsTooHigh, // 6082

    #[msg("Delayed unstake fee is too high")]
    DelayedUnstakeFeeIsTooHigh, // 6083

    #[msg("Withdraw stake account value is too low")]
    WithdrawStakeLamportsIsTooLow, // 6084

    /// when the remainder after a withdraw stake account is less than min_stake
    #[msg("Stake account remainder too low")]
    StakeAccountRemainderTooLow, // 6085
}
