use crate::{error::MarinadeError, require_lte};
use anchor_lang::prelude::*;

use std::fmt::Display;
#[cfg(feature = "no-entrypoint")]
use std::str::FromStr;
//-----------------------------------------------------
#[derive(
    Clone, Copy, Debug, Default, AnchorSerialize, AnchorDeserialize, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct Fee {
    pub basis_points: u32,
}

impl Display for Fee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // use integer division to avoid including f64 libs
        write!(
            f,
            "{}.{:0>2}%",
            self.basis_points / 100,
            self.basis_points % 100
        )
    }
}

impl Fee {
    pub const MAX_BASIS_POINTS: u32 = 10_000;

    pub const fn from_basis_points(basis_points: u32) -> Self {
        Self { basis_points }
    }

    pub fn check(&self) -> Result<()> {
        require_lte!(
            self.basis_points,
            Self::MAX_BASIS_POINTS,
            MarinadeError::BasisPointsOverflow
        );
        Ok(())
    }

    pub fn apply(&self, lamports: u64) -> u64 {
        // LMT no error possible
        (lamports as u128 * self.basis_points as u128 / Self::MAX_BASIS_POINTS as u128) as u64
    }
}

#[cfg(feature = "no-entrypoint")]
impl TryFrom<f64> for Fee {
    type Error = Error;

    fn try_from(n: f64) -> Result<Self> {
        let basis_points_i = (n * 100.0).floor() as i64; // 4.5% => 450 basis_points
        let basis_points = u32::try_from(basis_points_i)?;
        let fee = Fee::from_basis_points(basis_points);
        fee.check()?;
        Ok(fee)
    }
}

#[cfg(feature = "no-entrypoint")]
impl FromStr for Fee {
    type Err = Error; // TODO: better error

    fn from_str(s: &str) -> Result<Self> {
        f64::try_into(s.parse()?)
    }
}

/// FeeCents, same as Fee but / 1_000_000 instead of 10_000
/// 1 FeeCent = 0.0001%, 10_000 FeeCent = 1%, 1_000_000 FeeCent = 100%
#[derive(
    Clone, Copy, Debug, Default, AnchorSerialize, AnchorDeserialize, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct FeeCents {
    pub bp_cents: u32,
}

impl Display for FeeCents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // use integer division to avoid including f64 libs
        write!(
            f,
            "{}.{:0>4}%",
            self.bp_cents / 10_000,
            self.bp_cents % 10_000
        )
    }
}

impl FeeCents {
    pub const MAX_BP_CENTS: FeeCents = FeeCents::from_bp_cents(1_000_000); // 100%

    pub const fn from_bp_cents(bp_cents: u32) -> Self {
        Self { bp_cents }
    }

    pub fn check(&self) -> Result<()> {
        require_lte!(
            self,
            &Self::MAX_BP_CENTS,
            MarinadeError::BasisPointCentsOverflow
        );
        Ok(())
    }

    pub fn apply(&self, lamports: u64) -> u64 {
        // LMT no error possible
        (lamports as u128 * self.bp_cents as u128 / Self::MAX_BP_CENTS.bp_cents as u128) as u64
    }
}

#[cfg(feature = "no-entrypoint")]
impl TryFrom<f64> for FeeCents {
    type Error = Error;

    fn try_from(n: f64) -> Result<Self> {
        let bp_cents_i = (n * 10000.0).floor() as i64; // 4.5% => 45000 bp_cents
        let bp_cents = u32::try_from(bp_cents_i)?;
        let fee = Fee::from_bp_cents(bp_cents);
        fee.check()?;
        Ok(fee)
    }
}

#[cfg(feature = "no-entrypoint")]
impl FromStr for Fee {
    type Err = Error; // TODO: better error

    fn from_str(s: &str) -> Result<Self> {
        f64::try_into(s.parse()?)
    }
}
