use anchor_lang::prelude::*;

#[error]
pub enum CommonError {
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

    #[msg("1910 Invalid program id. For using program from another account please update id in the code")]
    InvalidProgramId = 6116,

    #[msg("FFA0 Unexpected account")]
    UnexpectedAccount = 65140,

    #[msg("CACF Calculation failure")]
    CalculationFailure = 51619,

    // #[error("B8A5 Wrong reserve address {got}. Must be {expected}")]
    // WrongReserveAddress { got: Pubkey, expected: Pubkey },

    // #[error("B5OA Wrong stake withdraw authority {got}. Must be {expected}")]
    // WrongStakeWithdrawAuthority { got: Pubkey, expected: Pubkey },

    // #[error("B51A Wrong stake deposit authority {got}. Must be {expected}")]
    // WrongStakeDepositAuthority { got: Pubkey, expected: Pubkey },
    #[msg("B3AA You can't deposit a stake-account with lockup")]
    AccountWithLockup = 45694,

    #[msg("2000 Number too low")]
    NumberTooLow = 7892,
    #[msg("2001 Number too high")]
    NumberTooHigh = 7893,

    #[msg("1100 Fee too high")]
    FeeTooHigh = 4052,

    #[msg("1101 Min fee > max fee")]
    FeesWrongWayRound = 4053,

    #[msg("1102 Liquidity target too low")]
    LiquidityTargetTooLow = 4054,

    #[msg("1103 Ticket not due. Wait more epochs")]
    TicketNotDue = 4055,

    #[msg("1104 Ticket not ready. Wait a few hours and try again")]
    TicketNotReady = 4056,

    #[msg("1105 Wrong Ticket Beneficiary")]
    WrongBeneficiary = 4057,

    #[msg("1199 Insufficient Liquidity in the Liquidity Pool")]
    InsufficientLiquidity = 4205,

    #[msg("BAD1 Invalid validator")]
    InvalidValidator = 47525,
}
