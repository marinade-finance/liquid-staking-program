use anchor_lang::prelude::*;

#[account]
#[derive(Debug)]
pub struct TicketAccountData {
    pub state_address: Pubkey, // instance of marinade state this ticket belongs to
    pub beneficiary: Pubkey,   // main account where to send SOL when claimed
    pub lamports_amount: u64,  // amount this ticked is worth
    pub created_epoch: u64, // epoch when this acc was created (epoch when delayed-unstake was requested)
}
