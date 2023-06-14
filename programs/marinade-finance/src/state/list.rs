use std::io::Cursor;

use anchor_lang::prelude::*;
use borsh::BorshSchema;
use std::convert::TryFrom;

use crate::{error::MarinadeError, require_lt};

#[derive(Default, Clone, AnchorSerialize, AnchorDeserialize, BorshSchema, Debug)]
pub struct List {
    pub account: Pubkey,
    pub item_size: u32,
    pub count: u32,
    // Unused
    pub _reserved1: Pubkey,
    pub _reserved2: u32,
}

impl List {
    pub fn new(
        discriminator: &[u8; 8],
        item_size: u32,
        account: Pubkey,
        data: &mut [u8],
    ) -> Result<Self> {
        let result = Self {
            account,
            item_size,
            count: 0,
            _reserved1: Pubkey::default(),
            _reserved2: 0,
        };
        result.init_account(discriminator, data)?;
        Ok(result)
    }

    pub fn bytes_for(item_size: u32, count: u32) -> u32 {
        8 + count * item_size
    }

    pub fn capacity_of(item_size: u32, account_len: usize) -> u32 {
        (account_len as u32 - 8) / item_size
    }

    fn init_account(&self, discriminator: &[u8; 8], data: &mut [u8]) -> Result<()> {
        assert_eq!(self.count, 0);
        require_gte!(
            data.len(),
            8,
            anchor_lang::error::ErrorCode::AccountDiscriminatorNotFound
        );
        if data[0..8] != [0; 8] {
            return err!(anchor_lang::error::ErrorCode::AccountDiscriminatorAlreadySet);
        }

        data[0..8].copy_from_slice(discriminator);

        Ok(())
    }

    pub fn item_size(&self) -> u32 {
        self.item_size
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn capacity(&self, account_len: usize) -> Result<u32> {
        Ok(u32::try_from(
            account_len
                .checked_sub(8)
                .ok_or(ProgramError::AccountDataTooSmall)?,
        )
        .map_err(|_| error!(MarinadeError::CalculationFailure))?
        .checked_div(self.item_size())
        .unwrap_or(std::u32::MAX)) // for zst element (why you are using it in list?)
    }

    pub fn get<I: AnchorDeserialize>(&self, data: &[u8], index: u32) -> Result<I> {
        require_lt!(index, self.len(), MarinadeError::ListIndexOutOfBounds);

        let start = 8 + (index * self.item_size()) as usize;
        I::deserialize(&mut &data[start..(start + self.item_size() as usize)]).map_err(|err| {
            Error::from(ProgramError::BorshIoError(err.to_string())).with_source(source!())
        })
    }

    pub fn set<I: AnchorSerialize>(&self, data: &mut [u8], index: u32, item: I) -> Result<()> {
        require_lt!(index, self.len(), MarinadeError::ListIndexOutOfBounds);

        let start = 8 + (index * self.item_size()) as usize;
        let mut cursor = Cursor::new(&mut data[start..(start + self.item_size() as usize)]);
        item.serialize(&mut cursor)?;

        Ok(())
    }

    pub fn push<I: AnchorSerialize>(&mut self, data: &mut [u8], item: I) -> Result<()> {
        let capacity = self.capacity(data.len())?;
        require_lt!(self.len(), capacity, MarinadeError::ListOverflow);

        let start = 8 + (self.len() * self.item_size()) as usize;
        let mut cursor = Cursor::new(&mut data[start..(start + self.item_size() as usize)]);
        item.serialize(&mut cursor)?;

        self.count += 1;

        Ok(())
    }

    pub fn remove(&mut self, data: &mut [u8], index: u32) -> Result<()> {
        require_lt!(index, self.len(), MarinadeError::ListIndexOutOfBounds);

        self.count -= 1;
        if index == self.count {
            return Ok(());
        }
        let start = 8 + (index * self.item_size()) as usize;
        let last_item_start = 8 + (self.count * self.item_size()) as usize;
        data.copy_within(
            last_item_start..last_item_start + self.item_size() as usize,
            start,
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use anchor_lang::prelude::*;

    use super::List;

    #[test]
    fn test_remove() -> Result<()> {
        const COUNT: usize = 10;
        for remove_index in 0..COUNT {
            let mut list_data = [0; COUNT + 8];
            let list_account = Pubkey::new_unique();
            let discriminator = &[1, 2, 3, 4, 5, 6, 7, 8];
            let mut list = List::new(discriminator, 1u32, list_account, &mut list_data)?;
            for i in 0..COUNT {
                list.push::<u8>(&mut list_data, 9 + i as u8)?;
            }
            assert_eq!(list.len(), COUNT as u32);
            for i in 0..COUNT {
                assert_eq!(list.get::<u8>(&list_data, i as u32)?, 9 + i as u8);
            }

            list.remove(&mut list_data, remove_index as u32)?;
            assert_eq!(list.len(), COUNT as u32 - 1);
            let expected_set: BTreeSet<u8> = (0..COUNT)
                .filter(|i| *i as usize != remove_index)
                .map(|x| (x + 9) as u8)
                .collect();
            let result_set = (0..list.len())
                .map(|i| list.get::<u8>(&list_data, i as u32))
                .collect::<Result<BTreeSet<u8>>>()?;

            assert_eq!(expected_set, result_set);
        }
        Ok(())
    }
}
