//! Conversions between anchor's solana-program 2.x types and litesvm's address/instruction types.

use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};

pub type AnchorPubkey = anchor_lang::prelude::Pubkey;

pub fn to_addr(p: &AnchorPubkey) -> Address {
    Address::new_from_array(p.to_bytes())
}

pub fn to_anchor(a: &Address) -> AnchorPubkey {
    AnchorPubkey::new_from_array(a.to_bytes())
}

/// Convert any instruction type with `program_id`, `accounts[{pubkey,is_signer,is_writable}]`, `data`.
#[macro_export]
macro_rules! convert_ix {
    ($ix:expr) => {{
        let i = $ix;
        ::solana_instruction::Instruction {
            program_id: ::solana_address::Address::new_from_array(i.program_id.to_bytes()),
            accounts: i
                .accounts
                .iter()
                .map(|m| ::solana_instruction::AccountMeta {
                    pubkey: ::solana_address::Address::new_from_array(m.pubkey.to_bytes()),
                    is_signer: m.is_signer,
                    is_writable: m.is_writable,
                })
                .collect(),
            data: i.data.clone(),
        }
    }};
}

/// Build an instruction for the Marinade program from anchor-generated accounts + data.
pub fn marinade_ix<A, D>(accounts: A, data: D) -> Instruction
where
    A: anchor_lang::ToAccountMetas,
    D: anchor_lang::InstructionData,
{
    Instruction {
        program_id: to_addr(&marinade_finance::ID),
        accounts: accounts
            .to_account_metas(None)
            .into_iter()
            .map(|m| AccountMeta {
                pubkey: to_addr(&m.pubkey),
                is_signer: m.is_signer,
                is_writable: m.is_writable,
            })
            .collect(),
        data: data.data(),
    }
}
