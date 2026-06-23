//! Flash-loan provider abstraction.
//!
//! A provider supplies (a) a fee quote and (b) the on-chain move-call coordinates
//! for borrow/repay, so the PTB builder can wrap any arbitrage route in a
//! borrow…repay pair without the arbitrage/scanner logic knowing which lender is
//! used. Add a new lender by implementing [`FlashLoanProvider`] and registering it
//! in [`provider_from`] — nothing in `scanner`/`ptb` changes.

/// Fee basis-points denominator — identical to `flash.move`'s `FEE_DENOM`.
pub const FEE_DENOM: u64 = 10_000;

/// Coordinates of one Move call (`package::module::function`). The live PTB builder
/// turns these into `ProgrammableTransaction` MoveCall commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveCallSpec {
    pub package: String,
    pub module: String,
    pub function: String,
}

/// Ceil(`amount` * `fee_bps` / FEE_DENOM), computed in u128 to avoid overflow.
/// Bit-for-bit identical to `flash::fee_amount` on-chain, so off-chain sizing never
/// under-estimates the repayment.
#[must_use]
pub fn quote_fee_bps(amount: u64, fee_bps: u64) -> u64 {
    let n = u128::from(amount) * u128::from(fee_bps);
    n.div_ceil(u128::from(FEE_DENOM)) as u64
}

/// A pluggable flash-loan lender. Implementations: [`MockProvider`] (the in-package
/// reference vault) and future Scallop / Navi / Suilend adapters.
pub trait FlashLoanProvider {
    fn name(&self) -> &str;
    fn fee_bps(&self) -> u64;

    /// Fee owed to borrow `amount` (rounded up, matches `flash::fee_amount`).
    fn quote_fee(&self, amount: u64) -> u64 {
        quote_fee_bps(amount, self.fee_bps())
    }
    /// Total that must be repaid for a loan of `amount` (= amount + fee).
    fn repay_total(&self, amount: u64) -> u64 {
        amount.saturating_add(self.quote_fee(amount))
    }

    /// Shared lender/vault object id to pass to borrow/repay.
    fn lender_object_id(&self) -> &str;
    /// Move call for borrow: `(lender, amount, ctx) -> (Coin<T>, FlashReceipt)`.
    fn borrow_call(&self) -> MoveCallSpec;
    /// Move call for repay: `(lender, receipt, payment, ctx) -> Coin<T>` (the change).
    fn repay_call(&self) -> MoveCallSpec;
    /// All current providers are single-asset and generic over the borrowed `<T>`.
    fn needs_type_arg(&self) -> bool {
        true
    }
}

/// Reference provider backed by the in-package `arbitrage_system::flash` vault.
/// Fully functional (used by the Move tests and any local/testnet deployment).
#[derive(Clone, Debug)]
pub struct MockProvider {
    pub package_id: String,
    pub lender_id: String,
    pub fee_bps: u64,
}

impl MockProvider {
    pub fn new(package_id: impl Into<String>, lender_id: impl Into<String>, fee_bps: u64) -> Self {
        Self {
            package_id: package_id.into(),
            lender_id: lender_id.into(),
            fee_bps,
        }
    }
}

impl FlashLoanProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn fee_bps(&self) -> u64 {
        self.fee_bps
    }
    fn lender_object_id(&self) -> &str {
        &self.lender_id
    }
    fn borrow_call(&self) -> MoveCallSpec {
        MoveCallSpec {
            package: self.package_id.clone(),
            module: "flash".into(),
            function: "borrow".into(),
        }
    }
    fn repay_call(&self) -> MoveCallSpec {
        MoveCallSpec {
            package: self.package_id.clone(),
            module: "flash".into(),
            function: "repay".into(),
        }
    }
}

/// Construct a provider by name. Returns `None` for unknown names (flash disabled).
///
/// To connect a real lender (Scallop / Navi / Suilend): implement
/// `FlashLoanProvider` with that protocol's package id and its
/// borrow/repay entry functions, then add a match arm here. See
/// `docs/flash-loan-design.md` for each protocol's exact API.
pub fn provider_from(
    name: &str,
    package_id: &str,
    lender_id: &str,
    fee_bps: u64,
) -> Option<Box<dyn FlashLoanProvider + Send + Sync>> {
    match name {
        "mock" => Some(Box::new(MockProvider::new(package_id, lender_id, fee_bps))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_quote_is_ceil_and_matches_move() {
        assert_eq!(quote_fee_bps(10, 30), 1); // 0.03 -> 1
        assert_eq!(quote_fee_bps(1_000, 30), 3); // exact
        assert_eq!(quote_fee_bps(0, 30), 0);
        assert_eq!(quote_fee_bps(1, 1), 1); // 0.0001 -> 1 (never zero on a real loan)
                                            // no overflow at large sizes
        assert_eq!(quote_fee_bps(1_000_000_000_000, 9), 900_000_000);
    }

    #[test]
    fn repay_total_includes_fee() {
        let p = MockProvider::new("0xpkg", "0xlender", 30);
        assert_eq!(p.quote_fee(10), 1);
        assert_eq!(p.repay_total(10), 11);
        assert_eq!(p.repay_total(1_000), 1_003);
    }

    #[test]
    fn mock_provider_emits_flash_calls() {
        let p = MockProvider::new("0xpkg", "0xlender", 30);
        assert_eq!(
            p.borrow_call(),
            MoveCallSpec {
                package: "0xpkg".into(),
                module: "flash".into(),
                function: "borrow".into()
            }
        );
        assert_eq!(p.repay_call().function, "repay");
        assert_eq!(p.lender_object_id(), "0xlender");
        assert!(p.needs_type_arg());
    }

    #[test]
    fn registry_resolves_mock_only() {
        assert!(provider_from("mock", "0xp", "0xl", 30).is_some());
        assert!(provider_from("scallop", "0xp", "0xl", 30).is_none());
    }
}
