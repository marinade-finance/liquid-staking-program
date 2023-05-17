use anchor_lang::prelude::*;

// NOTE: Anchor 0.27 adds 6000 for user error codes)
// (old Anchor 0.18 added 300 for user error codes)
#[error_code]
pub enum MarinadeError {
    #[msg("Wrong reserve owner. Must be a system account")]
    WrongReserveOwner,
    #[msg("Reserve must have no data, but has data")]
    NonEmptyReserveData,
    #[msg("Invalid initial reserve lamports")]
    InvalidInitialReserveLamports,
    #[msg("Zero validator chunk size")]
    ZeroValidatorChunkSize,
    #[msg("Too big validator chunk size")]
    TooBigValidatorChunkSize,
    #[msg("Zero credit chunk size")]
    ZeroCreditChunkSize,
    #[msg("Too big credit chunk size")]
    TooBigCreditChunkSize,
    #[msg("Too low credit fee")]
    TooLowCreditFee,
    #[msg("Invalid mint authority")]
    InvalidMintAuthority,
    #[msg("Non empty initial mint supply")]
    MintHasInitialSupply,
    #[msg("Invalid owner fee state")]
    InvalidOwnerFeeState,

    #[msg("Invalid program id. For using program from another account please update id in the code")]
    InvalidProgramId,

    #[msg("Unexpected account")]
    UnexpectedAccount,

    #[msg("Calculation failure")]
    CalculationFailure,

    #[msg("You can't deposit a stake-account with lockup")]
    AccountWithLockup,

    #[msg("Number too low")]
    NumberTooLow,
    #[msg("Number too high")]
    NumberTooHigh,

    #[msg("Fee too high")]
    FeeTooHigh,

    #[msg("Min fee > max fee")]
    FeesWrongWayRound,

    #[msg("Liquidity target too low")]
    LiquidityTargetTooLow,

    #[msg("Ticket not due. Wait more epochs")]
    TicketNotDue,

    #[msg("Ticket not ready. Wait a few hours and try again")]
    TicketNotReady,

    #[msg("Wrong Ticket Beneficiary")]
    WrongBeneficiary,

    #[msg("Stake Account not updated yet")]
    StakeAccountNotUpdatedYet,

    #[msg("Stake Account not delegated")]
    StakeNotDelegated,

    #[msg("Stake Account is emergency unstaking")]
    StakeAccountIsEmergencyUnstaking,

    #[msg("Insufficient Liquidity in the Liquidity Pool")]
    InsufficientLiquidity,

    #[msg("Invalid validator")]
    InvalidValidator,

    #[msg("Invalid admin authority")]
    InvalidAdminAuthority,
}
