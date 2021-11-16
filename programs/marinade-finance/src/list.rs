use std::io::Cursor;

use anchor_lang::prelude::*;
use borsh::BorshSchema;
use std::convert::TryFrom;

use crate::error::CommonError;

#[derive(Default, Clone, AnchorSerialize, AnchorDeserialize, BorshSchema, Debug)]
pub struct List {
    pub account: Pubkey,
    pub item_size: u32,
    pub count: u32,
    // For chunked change account
    pub new_account: Pubkey,
    pub copied_count: u32,
}

impl List {
    pub fn new(
        discriminator: &[u8; 8],
        item_size: u32,
        account: Pubkey,
        data: &mut [u8],
        list_name: &str,
    ) -> Result<Self, ProgramError> {
        let result = Self {
            account,
            item_size,
            count: 0,
            new_account: Pubkey::default(),
            copied_count: 0,
        };
        result.init_account(discriminator, data, list_name)?;
        Ok(result)
    }

    pub fn bytes_for(item_size: u32, count: u32) -> u32 {
        8 + count * item_size
    }

    pub fn capacity_of(item_size: u32, account_len: usize) -> u32 {
        (account_len as u32 - 8) / item_size
    }

    fn init_account(
        &self,
        discriminator: &[u8; 8],
        data: &mut [u8],
        list_name: &str,
    ) -> ProgramResult {
        assert_eq!(self.count, 0);
        if data.len() < 8 {
            msg!(
                "{} account must have at least 8 bytes of storage",
                list_name
            );
            return Err(ProgramError::AccountDataTooSmall);
        }
        if data[0..8] != [0; 8] {
            msg!("{} account is already initialized", list_name);
            return Err(ProgramError::AccountAlreadyInitialized);
        }

        data[0..8].copy_from_slice(discriminator);

        Ok(())
    }

    /*
    pub fn check_account<'info>(
        &self,
        account: &AccountInfo<'info>,
        list_name: &str,
    ) -> ProgramResult {
        check_address(account.key, &self.account, list_name)?;
        let data = account.data.borrow();
        if data.len() < 8 {
            msg!(
                "{} account must have at least 8 bytes of storage",
                list_name
            );
            return Err(ProgramError::AccountDataTooSmall);
        }

        if data[0..8] != D::discriminator() {
            msg!(
                "{} account must have discriminator {:?}. Got {:?}",
                list_name,
                D::discriminator(),
                &data[0..8]
            );
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(())
    }*/

    pub fn item_size(&self) -> u32 {
        self.item_size
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn is_changing_account(&self) -> bool {
        self.new_account != Pubkey::default()
    }

    pub fn capacity(&self, account_len: usize) -> Result<u32, ProgramError> {
        Ok(u32::try_from(
            account_len
                .checked_sub(8)
                .ok_or(ProgramError::AccountDataTooSmall)?,
        )
        .map_err(|_| ProgramError::from(CommonError::CalculationFailure))?
        .checked_div(self.item_size())
        .unwrap_or(std::u32::MAX)) // for zst element (why you are using it in list?)
    }

    pub fn get<I: AnchorDeserialize>(
        &self,
        data: &[u8],
        index: u32,
        list_name: &str,
    ) -> Result<I, ProgramError> {
        if index >= self.len() {
            msg!(
                "list {} index out of bounds ({}/{})",
                list_name,
                index,
                self.len()
            );
            return Err(ProgramError::InvalidArgument);
        }
        let start = 8 + (index * self.item_size()) as usize;
        I::deserialize(&mut &data[start..(start + self.item_size() as usize)])
            .map_err(|err| ProgramError::BorshIoError(err.to_string()))
    }

    pub fn set<I: AnchorSerialize>(
        &self,
        data: &mut [u8],
        index: u32,
        item: I,
        list_name: &str,
    ) -> ProgramResult {
        if self.new_account != Pubkey::default() {
            msg!("Can not modify list {} while changing list's account");
            return Err(ProgramError::InvalidAccountData);
        }
        if index >= self.len() {
            msg!(
                "list {} index out of bounds ({}/{})",
                list_name,
                index,
                self.len()
            );
            return Err(ProgramError::InvalidArgument);
        }

        let start = 8 + (index * self.item_size()) as usize;
        let mut cursor = Cursor::new(&mut data[start..(start + self.item_size() as usize)]);
        item.serialize(&mut cursor)?;

        Ok(())
    }

    pub fn push<I: AnchorSerialize>(
        &mut self,
        data: &mut [u8],
        item: I,
        list_name: &str,
    ) -> ProgramResult {
        if self.new_account != Pubkey::default() {
            msg!("Can not modify list {} while changing list's account");
            return Err(ProgramError::InvalidAccountData);
        }
        let capacity = self.capacity(data.len())?;
        if self.len() >= capacity {
            msg!("list {} with capacity {} is full", list_name, capacity);
            return Err(ProgramError::AccountDataTooSmall);
        }

        let start = 8 + (self.len() * self.item_size()) as usize;
        let mut cursor = Cursor::new(&mut data[start..(start + self.item_size() as usize)]);
        item.serialize(&mut cursor)?;

        self.count += 1;

        Ok(())
    }

    pub fn remove(&mut self, data: &mut [u8], index: u32, list_name: &str) -> ProgramResult {
        if self.new_account != Pubkey::default() {
            msg!("Can not modify list {} while changing list's account");
            return Err(ProgramError::InvalidAccountData);
        }
        if index >= self.len() {
            msg!(
                "list {} remove out of bounds ({}/{})",
                list_name,
                index,
                self.len()
            );
            return Err(ProgramError::InvalidArgument);
        }

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

    /*
    pub fn change_account<'info>(
        &mut self,
        old_account: &AccountInfo<'info>,
        new_account: &AccountInfo<'info>,
        max_copy_count: u32,
        list_name: &str,
    ) -> Result<bool, ProgramError> {
        self.check_account(old_account, list_name)?;
        let data_size = 8 + (self.len() * self.item_size()) as usize;
        let mut new_data = new_account.data.borrow_mut();
        if self.new_account != *new_account.key {
            if self.new_account != Pubkey::default() {
                msg!(
                    "list {} already changing account into {}",
                    list_name,
                    self.new_account
                );
                return Err(ProgramError::InvalidArgument);
            }
            if new_data.len() < data_size {
                msg!(
                    "Account {} is too small for copying list {}. At least {} bytes needed",
                    new_account.key,
                    list_name,
                    data_size
                );
                return Err(ProgramError::AccountDataTooSmall);
            }
            self.init_account(new_account, list_name)?;

            self.new_account = *new_account.key;
            self.copied_count = 0;
        }

        let copy_count = max_copy_count.min(self.len() - self.copied_count);

        let start = 8 + (self.copied_count * self.item_size()) as usize;
        let stop = start + (self.item_size() * copy_count) as usize;
        new_data[start..stop].copy_from_slice(&old_account.data.borrow()[start..stop]);
        self.copied_count += copy_count;
        if self.copied_count == self.len() {
            self.account = self.new_account;
            self.new_account = Pubkey::default();
            self.copied_count = 0;
            Ok(true)
        } else {
            Ok(false)
        }
    }*/

    /*
    pub fn iter<'a, 'info>(
        &'a self,
        account: &'a AccountInfo<'info>,
        list_name: &'a str,
    ) -> Iter<'a, 'info, D, I, S> {
        Iter {
            list: self,
            account,
            index: 0,
            list_name,
        }
    }*/
}
/*
pub struct Iter<'a, 'info, D, I, S> {
    pub list: &'a List<D, I, S>,
    pub account: &'a AccountInfo<'info>,
    pub index: u32,
    list_name: &'a str,
}

impl<'a, 'info, D, I, S> Iterator for Iter<'a, 'info, D, I, S>
where
    D: Discriminator,
    I: AnchorSerialize + AnchorDeserialize,
    S: SerializedSize,
{
    type Item = Result<I, ProgramError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.list.len() {
            let result = self.list.get(self.account, self.index, self.list_name);
            self.index += 1;
            Some(result)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = (self.list.len() - self.index) as usize;
        (count, Some(count))
    }

    fn count(self) -> usize
    where
        Self: Sized,
    {
        (self.list.len() - self.index) as usize
    }

    fn last(self) -> Option<Self::Item>
    where
        Self: Sized,
    {
        if self.index < self.list.len() {
            Some(
                self.list
                    .get(self.account, self.list.len() - 1, self.list_name),
            )
        } else {
            None
        }
    }
}

impl<'a, 'info, D, I, S> ExactSizeIterator for Iter<'a, 'info, D, I, S>
where
    D: Discriminator,
    I: AnchorSerialize + AnchorDeserialize,
    S: SerializedSize,
{
}
*/

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use anchor_lang::prelude::{ProgramError, ProgramResult, Pubkey};

    use crate::list::List;

    #[test]
    fn test_remove() -> ProgramResult {
        const COUNT: usize = 10;
        for remove_index in 0..COUNT {
            let mut list_data = [0; COUNT + 8];
            let list_account = Pubkey::new_unique();
            let discriminator = &[1, 2, 3, 4, 5, 6, 7, 8];
            let mut list = List::new(
                discriminator,
                1u32,
                list_account,
                &mut list_data,
                "test_list",
            )?;
            for i in 0..COUNT {
                list.push::<u8>(&mut list_data, 9 + i as u8, "test_list")?;
            }
            assert_eq!(list.len(), COUNT as u32);
            for i in 0..COUNT {
                assert_eq!(
                    list.get::<u8>(&list_data, i as u32, "test_list")?,
                    9 + i as u8
                );
            }

            list.remove(&mut list_data, remove_index as u32, "test_list")?;
            assert_eq!(list.len(), COUNT as u32 - 1);
            let expected_set: BTreeSet<u8> = (0..COUNT)
                .filter(|i| *i as usize != remove_index)
                .map(|x| (x + 9) as u8)
                .collect();
            let result_set = (0..list.len())
                .map(|i| list.get::<u8>(&list_data, i as u32, "test_list"))
                .collect::<Result<BTreeSet<u8>, ProgramError>>()?;

            assert_eq!(expected_set, result_set);
        }
        Ok(())
    }
}
