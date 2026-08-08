use serde::{Deserialize, Serialize};

use crate::{CreditReservationId, ExecutionId, PriceQuoteId, TaskId, Timestamp, UserId};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CreditAmount(u64);

impl CreditAmount {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreditAccount {
    pub user_id: UserId,
    pub available: CreditAmount,
    pub reserved: CreditAmount,
}

impl CreditAccount {
    /// Moves available credit into the reserved balance.
    ///
    /// # Errors
    ///
    /// Returns [`CreditError::InsufficientAvailable`] when the account cannot
    /// cover the amount, or [`CreditError::Overflow`] on arithmetic overflow.
    pub fn reserve(&mut self, amount: CreditAmount) -> Result<(), CreditError> {
        if self.available < amount {
            return Err(CreditError::InsufficientAvailable);
        }
        self.available = CreditAmount(self.available.0 - amount.0);
        self.reserved = CreditAmount(
            self.reserved
                .0
                .checked_add(amount.0)
                .ok_or(CreditError::Overflow)?,
        );
        Ok(())
    }

    /// Permanently consumes reserved credit after a successful execution.
    ///
    /// # Errors
    ///
    /// Returns [`CreditError::InsufficientReserved`] when the reservation is
    /// smaller than the requested amount.
    pub fn commit(&mut self, amount: CreditAmount) -> Result<(), CreditError> {
        if self.reserved < amount {
            return Err(CreditError::InsufficientReserved);
        }
        self.reserved = CreditAmount(self.reserved.0 - amount.0);
        Ok(())
    }

    /// Returns reserved credit to the available balance.
    ///
    /// # Errors
    ///
    /// Returns [`CreditError::InsufficientReserved`] when the reservation is
    /// smaller than the requested amount, or [`CreditError::Overflow`] on
    /// arithmetic overflow.
    pub fn release(&mut self, amount: CreditAmount) -> Result<(), CreditError> {
        if self.reserved < amount {
            return Err(CreditError::InsufficientReserved);
        }
        self.reserved = CreditAmount(self.reserved.0 - amount.0);
        self.available = CreditAmount(
            self.available
                .0
                .checked_add(amount.0)
                .ok_or(CreditError::Overflow)?,
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CreditError {
    #[error("available credit is insufficient")]
    InsufficientAvailable,
    #[error("reserved credit is insufficient")]
    InsufficientReserved,
    #[error("credit arithmetic overflowed")]
    Overflow,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PriceQuote {
    pub id: PriceQuoteId,
    pub task_id: TaskId,
    pub amount: CreditAmount,
    pub pricing_revision: String,
    pub reason: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditReservationState {
    Reserved,
    Committed,
    Released,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreditReservation {
    pub id: CreditReservationId,
    pub user_id: UserId,
    pub quote_id: PriceQuoteId,
    pub execution_id: ExecutionId,
    pub amount: CreditAmount,
    pub state: CreditReservationState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_commit_and_release_preserve_ledger_invariants() {
        let mut account = CreditAccount {
            user_id: UserId::new(),
            available: CreditAmount::new(100),
            reserved: CreditAmount::ZERO,
        };

        account.reserve(CreditAmount::new(30)).unwrap();
        assert_eq!(account.available.value(), 70);
        assert_eq!(account.reserved.value(), 30);

        account.release(CreditAmount::new(10)).unwrap();
        account.commit(CreditAmount::new(20)).unwrap();
        assert_eq!(account.available.value(), 80);
        assert_eq!(account.reserved.value(), 0);
    }

    #[test]
    fn cannot_reserve_more_than_available() {
        let mut account = CreditAccount {
            user_id: UserId::new(),
            available: CreditAmount::new(5),
            reserved: CreditAmount::ZERO,
        };
        assert_eq!(
            account.reserve(CreditAmount::new(6)),
            Err(CreditError::InsufficientAvailable)
        );
    }
}
