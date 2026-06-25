use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    account::{Money, VenueAccountError, VenueAccountStore},
    market::AssetId,
    model::AccountId,
};

pub type TransferId = u64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VenueTransferKind {
    Deposit,
    Withdrawal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VenueTransferStatus {
    Pending,
    Completed,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VenueTransfer {
    pub transfer_id: TransferId,
    pub kind: VenueTransferKind,
    pub account_id: AccountId,
    pub asset_id: AssetId,
    pub amount: Money,
    pub requested_at_step: u64,
    pub available_after_step: u64,
    pub completed_at_step: Option<u64>,
    pub status: VenueTransferStatus,
    pub reject_reason: Option<VenueTransferRejectReason>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VenueTransferRejectReason {
    NonPositiveAmount,
    InsufficientAvailableBalance,
    InsufficientPortfolioBalance,
    AssetNotAcceptedByVenue,
    AssetNotWithdrawableFromVenue,
    BalanceOverflow,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VenueTransferStore {
    next_transfer_id: TransferId,
    transfers: BTreeMap<TransferId, VenueTransfer>,
}

impl VenueTransferStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit_deposit(
        &mut self,
        accounts: &mut VenueAccountStore,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
        requested_at_step: u64,
        delay_steps: u64,
    ) -> VenueTransfer {
        let asset_id = asset_id.into();
        let transfer_id = self.take_transfer_id();
        let available_after_step = requested_at_step + delay_steps;
        let mut transfer = VenueTransfer {
            transfer_id,
            kind: VenueTransferKind::Deposit,
            account_id,
            asset_id,
            amount,
            requested_at_step,
            available_after_step,
            completed_at_step: None,
            status: VenueTransferStatus::Pending,
            reject_reason: None,
        };

        if amount <= 0 {
            transfer.status = VenueTransferStatus::Rejected;
            transfer.reject_reason = Some(VenueTransferRejectReason::NonPositiveAmount);
        } else if delay_steps == 0 {
            transfer = complete_deposit(accounts, transfer, requested_at_step);
        }

        self.transfers.insert(transfer_id, transfer.clone());
        transfer
    }

    pub fn submit_rejected_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
        requested_at_step: u64,
        reject_reason: VenueTransferRejectReason,
    ) -> VenueTransfer {
        let transfer_id = self.take_transfer_id();
        let transfer = VenueTransfer {
            transfer_id,
            kind: VenueTransferKind::Deposit,
            account_id,
            asset_id: asset_id.into(),
            amount,
            requested_at_step,
            available_after_step: requested_at_step,
            completed_at_step: None,
            status: VenueTransferStatus::Rejected,
            reject_reason: Some(reject_reason),
        };

        self.transfers.insert(transfer_id, transfer.clone());
        transfer
    }

    pub fn submit_withdrawal(
        &mut self,
        accounts: &mut VenueAccountStore,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
        requested_at_step: u64,
        delay_steps: u64,
    ) -> VenueTransfer {
        let asset_id = asset_id.into();
        let transfer_id = self.take_transfer_id();
        let available_after_step = requested_at_step + delay_steps;
        let mut transfer = VenueTransfer {
            transfer_id,
            kind: VenueTransferKind::Withdrawal,
            account_id,
            asset_id: asset_id.clone(),
            amount,
            requested_at_step,
            available_after_step,
            completed_at_step: None,
            status: VenueTransferStatus::Pending,
            reject_reason: None,
        };

        if amount <= 0 {
            transfer.status = VenueTransferStatus::Rejected;
            transfer.reject_reason = Some(VenueTransferRejectReason::NonPositiveAmount);
        } else if let Err(error) = accounts.reserve(account_id, asset_id, amount) {
            transfer.status = VenueTransferStatus::Rejected;
            transfer.reject_reason = Some(reject_reason_from_account_error(error));
        } else if delay_steps == 0 {
            transfer = complete_withdrawal(accounts, transfer, requested_at_step);
        }

        self.transfers.insert(transfer_id, transfer.clone());
        transfer
    }

    pub fn submit_rejected_withdrawal(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<AssetId>,
        amount: Money,
        requested_at_step: u64,
        reject_reason: VenueTransferRejectReason,
    ) -> VenueTransfer {
        let transfer_id = self.take_transfer_id();
        let transfer = VenueTransfer {
            transfer_id,
            kind: VenueTransferKind::Withdrawal,
            account_id,
            asset_id: asset_id.into(),
            amount,
            requested_at_step,
            available_after_step: requested_at_step,
            completed_at_step: None,
            status: VenueTransferStatus::Rejected,
            reject_reason: Some(reject_reason),
        };

        self.transfers.insert(transfer_id, transfer.clone());
        transfer
    }

    pub fn process_due(
        &mut self,
        accounts: &mut VenueAccountStore,
        current_step: u64,
    ) -> Vec<VenueTransfer> {
        let due_ids = self
            .transfers
            .iter()
            .filter_map(|(transfer_id, transfer)| {
                (transfer.status == VenueTransferStatus::Pending
                    && transfer.available_after_step <= current_step)
                    .then_some(*transfer_id)
            })
            .collect::<Vec<_>>();

        let mut completed = Vec::new();
        for transfer_id in due_ids {
            let Some(transfer) = self.transfers.remove(&transfer_id) else {
                continue;
            };
            let transfer = match transfer.kind {
                VenueTransferKind::Deposit => complete_deposit(accounts, transfer, current_step),
                VenueTransferKind::Withdrawal => {
                    complete_withdrawal(accounts, transfer, current_step)
                }
            };
            self.transfers.insert(transfer_id, transfer.clone());
            completed.push(transfer);
        }

        completed
    }

    pub fn transfers(&self) -> Vec<VenueTransfer> {
        self.transfers.values().cloned().collect()
    }

    fn take_transfer_id(&mut self) -> TransferId {
        let transfer_id = self.next_transfer_id;
        self.next_transfer_id += 1;
        transfer_id
    }
}

fn complete_deposit(
    accounts: &mut VenueAccountStore,
    mut transfer: VenueTransfer,
    completed_at_step: u64,
) -> VenueTransfer {
    match accounts.apply_signed_delta(
        transfer.account_id,
        transfer.asset_id.clone(),
        transfer.amount,
    ) {
        Ok(_) => {
            transfer.status = VenueTransferStatus::Completed;
            transfer.completed_at_step = Some(completed_at_step);
        }
        Err(error) => {
            transfer.status = VenueTransferStatus::Rejected;
            transfer.reject_reason = Some(reject_reason_from_account_error(error));
        }
    }
    transfer
}

fn complete_withdrawal(
    accounts: &mut VenueAccountStore,
    mut transfer: VenueTransfer,
    completed_at_step: u64,
) -> VenueTransfer {
    let release = accounts.release(
        transfer.account_id,
        transfer.asset_id.clone(),
        transfer.amount,
    );
    let debit = release.and_then(|_| {
        accounts.apply_delta(
            transfer.account_id,
            transfer.asset_id.clone(),
            -transfer.amount,
        )
    });

    match debit {
        Ok(_) => {
            transfer.status = VenueTransferStatus::Completed;
            transfer.completed_at_step = Some(completed_at_step);
        }
        Err(error) => {
            transfer.status = VenueTransferStatus::Rejected;
            transfer.reject_reason = Some(reject_reason_from_account_error(error));
        }
    }
    transfer
}

fn reject_reason_from_account_error(error: VenueAccountError) -> VenueTransferRejectReason {
    match error {
        VenueAccountError::BalanceOverflow => VenueTransferRejectReason::BalanceOverflow,
        VenueAccountError::NegativeAmount
        | VenueAccountError::InsufficientAvailableBalance
        | VenueAccountError::InsufficientReservedBalance => {
            VenueTransferRejectReason::InsufficientAvailableBalance
        }
    }
}
