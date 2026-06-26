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

/// Scallop call coordinates, **verified against the `scallop-io/sui-lending-protocol`
/// `protocol` package source** (module `protocol`, commit on the pinned clone). These
/// are the single source of truth for the move-calls the flash provider + liquidation
/// PTB emit; the `scallop_pins` tests fail loudly if our emitted coordinates drift.
///
///   flash_loan::borrow_flash_loan<T>(version, market, amount, ctx) -> (Coin<T>, FlashLoan<T>)
///   flash_loan::repay_flash_loan<T>(version, market, coin, loan, ctx)
///   liquidate::liquidate<Debt,Coll>(version, obligation, market, available_repay,
///       coin_decimals_registry, x_oracle, clock, ctx) -> (Coin<Debt> remain, Coin<Coll> seized)
pub mod scallop_pins {
    /// `protocol` package address (named-address `protocol`; matches the mainnet pkg).
    pub const PACKAGE_ADDR: &str =
        "0xefe8b36d5b2e43728cc323298626b83177803521d195cfb11e15b910e892fddf";
    pub const FLASH_MODULE: &str = "flash_loan";
    pub const BORROW_FN: &str = "borrow_flash_loan";
    pub const REPAY_FN: &str = "repay_flash_loan";
    pub const LIQUIDATE_MODULE: &str = "liquidate";
    pub const LIQUIDATE_FN: &str = "liquidate";
}

/// On-chain call shape of a provider's borrow/repay, so the live PTB builder can
/// order arguments + thread the receipt correctly. Providers differ structurally:
/// the in-package vault takes `(lender, amount)`; Scallop takes `(version, market,
/// amount)` and repays `(version, market, coin, loan)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashStyle {
    /// In-package `flash` vault. borrow(lender, amount) -> (Coin, FlashReceipt);
    /// repay(lender, receipt, payment) -> Coin (change).
    MockVault,
    /// Scallop. borrow_flash_loan(version, market, amount) -> (Coin, FlashLoan);
    /// repay_flash_loan(version, market, coin, loan). Needs the shared `Version` obj.
    Scallop,
}

/// A pluggable flash-loan lender. Implementations: [`MockProvider`] (the in-package
/// reference vault) and [`ScallopProvider`] (a live mainnet lender).
pub trait FlashLoanProvider {
    fn name(&self) -> &str;
    fn fee_bps(&self) -> u64;

    /// On-chain call shape (drives PTB argument ordering). Defaults to the vault.
    fn style(&self) -> FlashStyle {
        FlashStyle::MockVault
    }
    /// Extra shared-object ids the borrow/repay calls need beyond the lender object
    /// (e.g. Scallop's `Version`). Resolved fresh to `ObjectArg`s by the live builder.
    fn extra_object_ids(&self) -> Vec<String> {
        Vec::new()
    }

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

/// Scallop flash-loan provider (live mainnet lender).
///
/// `protocol::flash_loan::borrow_flash_loan<T>(version, market, amount)
///   -> (Coin<T>, FlashLoan<T>)` and
/// `repay_flash_loan<T>(version, market, coin, loan)`.
///
/// `package_id` is Scallop's published-at id; `market_id`/`version_id` are the shared
/// `Market` and `Version` objects (resolve fresh on-chain). `fee_bps` must match the
/// market's configured flash-loan fee so off-chain sizing nets the true repayment.
#[derive(Clone, Debug)]
pub struct ScallopProvider {
    pub package_id: String,
    pub market_id: String,
    pub version_id: String,
    pub fee_bps: u64,
}

impl ScallopProvider {
    pub fn new(
        package_id: impl Into<String>,
        market_id: impl Into<String>,
        version_id: impl Into<String>,
        fee_bps: u64,
    ) -> Self {
        Self {
            package_id: package_id.into(),
            market_id: market_id.into(),
            version_id: version_id.into(),
            fee_bps,
        }
    }
}

impl FlashLoanProvider for ScallopProvider {
    fn name(&self) -> &str {
        "scallop"
    }
    fn fee_bps(&self) -> u64 {
        self.fee_bps
    }
    fn style(&self) -> FlashStyle {
        FlashStyle::Scallop
    }
    fn extra_object_ids(&self) -> Vec<String> {
        vec![self.version_id.clone()]
    }
    fn lender_object_id(&self) -> &str {
        &self.market_id
    }
    fn borrow_call(&self) -> MoveCallSpec {
        MoveCallSpec {
            package: self.package_id.clone(),
            module: scallop_pins::FLASH_MODULE.into(),
            function: scallop_pins::BORROW_FN.into(),
        }
    }
    fn repay_call(&self) -> MoveCallSpec {
        MoveCallSpec {
            package: self.package_id.clone(),
            module: scallop_pins::FLASH_MODULE.into(),
            function: scallop_pins::REPAY_FN.into(),
        }
    }
}

/// Construct a provider by name. Returns `None` for unknown names (flash disabled).
/// `extra` carries the provider's secondary object id when it needs one (Scallop's
/// `Version`); ignored by the mock vault.
pub fn provider_from(
    name: &str,
    package_id: &str,
    lender_id: &str,
    fee_bps: u64,
    extra: &str,
) -> Option<Box<dyn FlashLoanProvider + Send + Sync>> {
    match name {
        "mock" => Some(Box::new(MockProvider::new(package_id, lender_id, fee_bps))),
        "scallop" => Some(Box::new(ScallopProvider::new(
            package_id, lender_id, extra, fee_bps,
        ))),
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
    fn registry_resolves_mock_and_scallop() {
        assert!(provider_from("mock", "0xp", "0xl", 30, "").is_some());
        assert!(provider_from("scallop", "0xpkg", "0xmarket", 30, "0xversion").is_some());
        assert!(provider_from("navi", "0xp", "0xl", 30, "").is_none());
    }

    #[test]
    fn mock_provider_style_is_vault() {
        let p = MockProvider::new("0xpkg", "0xlender", 30);
        assert_eq!(p.style(), FlashStyle::MockVault);
        assert!(p.extra_object_ids().is_empty());
    }

    #[test]
    fn scallop_provider_shape() {
        let p = ScallopProvider::new("0xpkg", "0xmarket", "0xversion", 9);
        assert_eq!(p.name(), "scallop");
        assert_eq!(p.style(), FlashStyle::Scallop);
        assert_eq!(p.lender_object_id(), "0xmarket"); // borrow/repay take the Market
        assert_eq!(p.extra_object_ids(), vec!["0xversion".to_string()]); // + Version
        assert_eq!(
            p.borrow_call(),
            MoveCallSpec {
                package: "0xpkg".into(),
                module: "flash_loan".into(),
                function: "borrow_flash_loan".into()
            }
        );
        assert_eq!(p.repay_call().function, "repay_flash_loan");
        // fee parity with the ceil formula
        assert_eq!(p.quote_fee(1_000_000), quote_fee_bps(1_000_000, 9));
    }

    /// Pin the verified Scallop coordinates. If anyone edits `scallop_pins` (or the
    /// provider stops using it), this fails loudly — guarding against silent drift from
    /// the values verified against Scallop source.
    #[test]
    fn scallop_signatures_pinned() {
        assert_eq!(scallop_pins::FLASH_MODULE, "flash_loan");
        assert_eq!(scallop_pins::BORROW_FN, "borrow_flash_loan");
        assert_eq!(scallop_pins::REPAY_FN, "repay_flash_loan");
        assert_eq!(scallop_pins::LIQUIDATE_MODULE, "liquidate");
        assert_eq!(scallop_pins::LIQUIDATE_FN, "liquidate");
        assert!(scallop_pins::PACKAGE_ADDR.starts_with("0xefe8b36d"));
        // the provider must emit exactly the pinned coordinates
        let p = ScallopProvider::new(scallop_pins::PACKAGE_ADDR, "0xmarket", "0xversion", 9);
        assert_eq!(p.borrow_call().module, scallop_pins::FLASH_MODULE);
        assert_eq!(p.borrow_call().function, scallop_pins::BORROW_FN);
        assert_eq!(p.repay_call().function, scallop_pins::REPAY_FN);
    }
}
