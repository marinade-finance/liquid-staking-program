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

    #[msg("Invalid program id. For using program from another account please update id in the code")]
    InvalidProgramId, // 6011

    #[msg("Unexpected account")]
    UnexpectedAccount, // 6012

    #[msg("Calculation failure")]
    CalculationFailure, // 6013

    #[msg("You can't deposit a stake-account with lockup")]
    AccountWithLockup, // 6014

    #[msg("Number too low")]
    NumberTooLow, // 6015
    #[msg("Number too high")]
    NumberTooHigh, // 6016

    #[msg("Fee too high")]
    FeeTooHigh, // 6017

    #[msg("Min fee > max fee")]
    FeesWrongWayRound, // 6018

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

    #[msg("Invalid validator")]
    InvalidValidator, // 6027

    #[msg("Invalid admin authority")]
    InvalidAdminAuthority, // 6028

    #[msg("Invalid stake list account discriminator")]
    InvalidStakeListDiscriminator, // 6030

    #[msg("Invalid validator list account discriminator")]
    InvalidValidatorListDiscriminator, // 6031
}
