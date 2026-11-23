use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use stringcan_core::FeeModel;

use crate::{
    AccountStatus, ChargeRequest, ChargeResult, ConnectAccounts, PaymentProcessor, ProcessorError,
    RefundRequest,
};

/// Deterministic in-memory processor and Connect provider. Customers whose ref
/// contains `decline` are declined; everything else succeeds with the model's fee.
pub struct FakeProcessor {
    fees: FeeModel,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    charges: HashMap<String, ChargeResult>,
    refunds: HashMap<String, String>,
    accounts: HashMap<String, String>,
    statuses: HashMap<String, AccountStatus>,
    charge_calls: usize,
}

impl FakeProcessor {
    pub fn new(fees: FeeModel) -> Self {
        FakeProcessor {
            fees,
            state: Mutex::new(State::default()),
        }
    }

    /// Distinct charges actually created (idempotent replays excluded).
    pub fn charges_created(&self) -> usize {
        self.state
            .lock()
            .expect("fake processor lock")
            .charges
            .len()
    }

    pub fn charge_calls(&self) -> usize {
        self.state.lock().expect("fake processor lock").charge_calls
    }

    pub fn set_status(&self, account: &str, status: AccountStatus) {
        self.state
            .lock()
            .expect("fake processor lock")
            .statuses
            .insert(account.to_owned(), status);
    }
}

#[async_trait]
impl PaymentProcessor for FakeProcessor {
    async fn charge(&self, req: &ChargeRequest) -> Result<ChargeResult, ProcessorError> {
        if req.gross <= 0 {
            return Err(ProcessorError::Invalid("amount must be positive"));
        }
        let mut st = self.state.lock().expect("fake processor lock");
        st.charge_calls += 1;
        if let Some(done) = st.charges.get(&req.idempotency_key) {
            return Ok(done.clone());
        }
        if req.customer_ref.contains("decline") {
            return Err(ProcessorError::Declined("card_declined".into()));
        }
        let result = ChargeResult {
            processor_ref: format!("pi_fake{}", st.charges.len() + 1),
            fee: self.fees.fee(req.gross),
        };
        st.charges
            .insert(req.idempotency_key.clone(), result.clone());
        Ok(result)
    }

    async fn refund(&self, req: &RefundRequest) -> Result<String, ProcessorError> {
        if req.amount <= 0 {
            return Err(ProcessorError::Invalid("amount must be positive"));
        }
        let mut st = self.state.lock().expect("fake processor lock");
        let n = st.refunds.len() + 1;
        Ok(st
            .refunds
            .entry(req.idempotency_key.clone())
            .or_insert_with(|| format!("re_fake{n}"))
            .clone())
    }
}

#[async_trait]
impl ConnectAccounts for FakeProcessor {
    async fn create_account(
        &self,
        idempotency_key: &str,
        _tenant_ref: &str,
    ) -> Result<String, ProcessorError> {
        // Derived from the key so ids are stable and unique across tenants in a shared test database.
        let suffix: String = idempotency_key
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(40)
            .collect();
        let id = format!("acct_fake{suffix}");
        self.state
            .lock()
            .expect("fake processor lock")
            .accounts
            .insert(idempotency_key.to_owned(), id.clone());
        Ok(id)
    }

    async fn onboarding_link(
        &self,
        account: &str,
        _refresh: &str,
        _return: &str,
    ) -> Result<String, ProcessorError> {
        Ok(format!("https://connect.stripe.test/setup/{account}"))
    }

    async fn account_status(&self, account: &str) -> Result<AccountStatus, ProcessorError> {
        Ok(self
            .state
            .lock()
            .expect("fake processor lock")
            .statuses
            .get(account)
            .copied()
            .unwrap_or_default())
    }
}
