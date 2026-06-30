//! Programmable Transaction Block construction for arbitrage — with flash loans.
//!
//! The command **plan** (ordered steps as data) is pure and unit-tested in the
//! default build. The live [`build`] fn (feature = "live") turns a plan into a real
//! `sui_types::ProgrammableTransaction`.
//!
//! Flash-arb PTB (one atomic transaction):
//! ```text
//!   (loan, frcpt)  = flash::borrow(lender, amount)            // borrowed capital
//!   (coin, arcpt)  = executor::begin(loan, min_profit)
//!   coin           = <adapter>::swap_exact_in_*(pool, coin, min_out)   // per hop
//!   ...
//!   proceeds       = executor::settle_and_return(arcpt, coin) // PROFIT GATE
//!   change         = flash::repay(lender, frcpt, proceeds)    // REPAYMENT GATE
//!   transfer(change, sender)                                  // keep the profit
//! ```
//! Two hot potatoes (`ArbReceipt`, `FlashReceipt`) must both be discharged in the
//! block; either gate aborting reverts the entire PTB. No owned capital is required
//! — the loan funds the whole route and the change is the net profit.

use crate::flashloan::{FlashLoanProvider, MoveCallSpec};
use crate::scanner::{Hop, Opportunity, Protocol};
use crate::types::Dex;

/// One step of the PTB, in execution order. Pure data — the live builder maps each
/// to a `sui_types` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PtbStep {
    /// `flash::borrow(lender, amount)` → (loan coin, FlashReceipt)
    FlashBorrow {
        call: MoveCallSpec,
        lender_id: String,
        amount: u64,
    },
    /// `executor::begin(coin, min_profit)` → (coin, ArbReceipt)
    Begin { min_profit: u64 },
    /// `<adapter>::swap_exact_in_*(pool, coin, min_out)` → coin
    Swap {
        module: String,
        function: String,
        pool_id: String,
        min_out: u64,
    },
    /// In-band oracle refresh required before a liquidation reads the protocol's price.
    /// VERIFIED expansion (doc §4): `vaa::parse_and_verify` →
    /// `pyth::create_authenticated_price_infos_using_accumulator` → per `feed`
    /// `pyth::update_single_price_feed` (paid, fresh `PriceInfoObject`) →
    /// `hot_potato_vector::destroy`. Scallop's `&XOracle` then reads the fresh feeds.
    PriceUpdate { feeds: Vec<String> },
    /// `<protocol>::liquidate(...)` → (remain debt coin, seized collateral coin). The
    /// seized collateral is then swapped back to the debt asset by the following hops.
    Liquidate {
        protocol: Protocol,
        obligation_id: String,
    },
    /// `executor::settle_and_return(receipt, coin)` → proceeds (enforces profit)
    SettleAndReturn,
    /// `flash::repay(lender, receipt, proceeds)` → change (enforces repayment)
    FlashRepay { call: MoveCallSpec },
    /// `executor::settle(receipt, coin)` — owned-capital path (transfers to sender)
    Settle,
    /// `TransferObjects([coin], sender)`
    TransferToSender,
}

/// Build the flash-loan arbitrage plan: borrow → begin → swaps → settle_and_return
/// → repay → transfer. `min_profit` must already include the flash fee + gas margin
/// (the scanner computes it) so the profit gate also guarantees repayment.
#[must_use]
pub fn flash_arb_plan(
    provider: &dyn FlashLoanProvider,
    opp: &Opportunity,
    min_profit: u64,
) -> Vec<PtbStep> {
    let mut steps = Vec::with_capacity(opp.route.len() + 5);
    steps.push(PtbStep::FlashBorrow {
        call: provider.borrow_call(),
        lender_id: provider.lender_object_id().to_string(),
        amount: opp.input_amount,
    });
    steps.push(PtbStep::Begin { min_profit });
    push_swaps(&mut steps, &opp.route);
    steps.push(PtbStep::SettleAndReturn);
    steps.push(PtbStep::FlashRepay {
        call: provider.repay_call(),
    });
    steps.push(PtbStep::TransferToSender);
    steps
}

/// Owned-capital plan (no flash loan): begin → swaps → settle. Kept for the
/// non-flash path and for comparison.
#[must_use]
pub fn owned_arb_plan(opp: &Opportunity, min_profit: u64) -> Vec<PtbStep> {
    let mut steps = Vec::with_capacity(opp.route.len() + 2);
    steps.push(PtbStep::Begin { min_profit });
    push_swaps(&mut steps, &opp.route);
    steps.push(PtbStep::Settle);
    steps
}

/// Liquidation plan: `[flash borrow] → begin → liquidate → swap-back → settle_and_return
/// → [flash repay] → transfer`. The seized collateral (from the liquidate leg) is
/// swapped to the debt asset by `opp.route`; the profit gate and flash repay are the
/// existing, unchanged gates. With `provider = None` the repay comes from owned capital
/// and the plan ends at `settle` (no flash). Mirrors `flash_arb_plan` — liquidation is
/// just another source on the same pipeline.
#[must_use]
pub fn liquidation_plan(
    provider: Option<&dyn FlashLoanProvider>,
    opp: &Opportunity,
    min_profit: u64,
) -> Vec<PtbStep> {
    let leg = opp
        .liquidation
        .as_ref()
        .expect("liquidation opportunity must carry a LiquidationLeg");
    let mut steps = Vec::with_capacity(opp.route.len() + 6);
    // Oracle MUST be refreshed in-band before liquidate reads it (Scallop x_oracle is
    // Pyth-backed and staleness-checked). The assets whose price liquidate reads are the
    // debt + collateral of the leg.
    steps.push(PtbStep::PriceUpdate {
        feeds: vec![leg.debt_type.clone(), leg.collateral_type.clone()],
    });
    if let Some(p) = provider {
        steps.push(PtbStep::FlashBorrow {
            call: p.borrow_call(),
            lender_id: p.lender_object_id().to_string(),
            amount: leg.repay_amount,
        });
    }
    steps.push(PtbStep::Begin { min_profit });
    steps.push(PtbStep::Liquidate {
        protocol: leg.protocol,
        obligation_id: leg.obligation_id.clone(),
    });
    push_swaps(&mut steps, &opp.route); // seized collateral → debt asset
    if let Some(p) = provider {
        steps.push(PtbStep::SettleAndReturn);
        steps.push(PtbStep::FlashRepay {
            call: p.repay_call(),
        });
        steps.push(PtbStep::TransferToSender);
    } else {
        steps.push(PtbStep::Settle);
    }
    steps
}

fn push_swaps(steps: &mut Vec<PtbStep>, route: &[Hop]) {
    for hop in route {
        let (module, function) = adapter_call(hop);
        steps.push(PtbStep::Swap {
            module: module.to_string(),
            function: function.to_string(),
            pool_id: hop.pool_id.clone(),
            min_out: 0, // per-hop slippage floor; end-to-end gate is the profit check
        });
    }
}

/// Map a hop to its adapter module + function per the adapter convention.
fn adapter_call(hop: &Hop) -> (&'static str, &'static str) {
    let module = match hop.dex {
        Dex::AmmV2 => "amm_v2_adapter",
        Dex::Cetus => "cetus_adapter",
        Dex::Turbos => "turbos_adapter",
    };
    let func = if hop.a_to_b {
        "swap_exact_in_a_to_b"
    } else {
        "swap_exact_in_b_to_a"
    };
    (module, func)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flashloan::MockProvider;
    use crate::scanner::{LiquidationLeg, OppKind, Opportunity, Protocol};
    use crate::types::Dex;

    fn liq_opp() -> Opportunity {
        Opportunity {
            kind: OppKind::Liquidation,
            liquidation: Some(LiquidationLeg {
                protocol: Protocol::Scallop,
                obligation_id: "0xob".into(),
                debt_type: "USDC".into(),
                collateral_type: "SUI".into(),
                repay_amount: 200_000_000,
                extra_object_ids: vec![],
            }),
            route: vec![Hop {
                pool_id: "0xPOOL".into(),
                dex: Dex::Cetus,
                token_in: "SUI".into(),
                token_out: "USDC".into(),
                a_to_b: true,
            }],
            input_amount: 200_000_000,
            output_amount: 216_000_000,
            hop_outputs: vec![216_000_000],
            gross_profit: 16_000_000,
            flash_fee: 60_000,
            net_profit: 15_000_000,
        }
    }

    fn opp() -> Opportunity {
        Opportunity {
            kind: OppKind::Arb,
            liquidation: None,
            route: vec![
                Hop {
                    pool_id: "0xAB".into(),
                    dex: Dex::AmmV2,
                    token_in: "A".into(),
                    token_out: "B".into(),
                    a_to_b: true,
                },
                Hop {
                    pool_id: "0xBC".into(),
                    dex: Dex::Turbos,
                    token_in: "B".into(),
                    token_out: "C".into(),
                    a_to_b: true,
                },
                Hop {
                    pool_id: "0xCA".into(),
                    dex: Dex::Cetus,
                    token_in: "C".into(),
                    token_out: "A".into(),
                    a_to_b: false,
                },
            ],
            input_amount: 1_000_000,
            output_amount: 1_050_000,
            hop_outputs: vec![1_020_000, 1_035_000, 1_050_000],
            gross_profit: 50_000,
            flash_fee: 300,
            net_profit: 41_700,
        }
    }

    #[test]
    fn flash_plan_has_borrow_begin_swaps_settle_repay_transfer() {
        let p = MockProvider::new("0xpkg", "0xlender", 30);
        let plan = flash_arb_plan(&p, &opp(), 8_000);
        // exact ordered shape
        assert!(matches!(
            plan[0],
            PtbStep::FlashBorrow {
                amount: 1_000_000,
                ..
            }
        ));
        assert!(matches!(plan[1], PtbStep::Begin { min_profit: 8_000 }));
        assert!(matches!(plan[2], PtbStep::Swap { .. }));
        assert!(matches!(plan[3], PtbStep::Swap { .. }));
        assert!(matches!(plan[4], PtbStep::Swap { .. }));
        assert!(matches!(plan[5], PtbStep::SettleAndReturn));
        assert!(matches!(plan[6], PtbStep::FlashRepay { .. }));
        assert!(matches!(plan[7], PtbStep::TransferToSender));
        assert_eq!(plan.len(), 8);
        // borrow precedes repay; settle precedes repay (profit gated before repayment)
        let borrow = plan
            .iter()
            .position(|s| matches!(s, PtbStep::FlashBorrow { .. }))
            .unwrap();
        let settle = plan
            .iter()
            .position(|s| matches!(s, PtbStep::SettleAndReturn))
            .unwrap();
        let repay = plan
            .iter()
            .position(|s| matches!(s, PtbStep::FlashRepay { .. }))
            .unwrap();
        assert!(borrow < settle && settle < repay);
    }

    #[test]
    fn swap_steps_map_to_correct_adapters() {
        let p = MockProvider::new("0xpkg", "0xlender", 30);
        let plan = flash_arb_plan(&p, &opp(), 1);
        let swaps: Vec<_> = plan
            .iter()
            .filter_map(|s| match s {
                PtbStep::Swap {
                    module, function, ..
                } => Some((module.as_str(), function.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            swaps,
            vec![
                ("amm_v2_adapter", "swap_exact_in_a_to_b"),
                ("turbos_adapter", "swap_exact_in_a_to_b"),
                ("cetus_adapter", "swap_exact_in_b_to_a"),
            ]
        );
    }

    #[test]
    fn owned_plan_has_no_flash_steps() {
        let plan = owned_arb_plan(&opp(), 1);
        assert!(matches!(plan.first(), Some(PtbStep::Begin { .. })));
        assert!(matches!(plan.last(), Some(PtbStep::Settle)));
        assert!(!plan
            .iter()
            .any(|s| matches!(s, PtbStep::FlashBorrow { .. } | PtbStep::FlashRepay { .. })));
    }

    #[test]
    fn flash_liquidation_plan_shape() {
        let p = MockProvider::new("0xpkg", "0xlender", 30);
        let plan = liquidation_plan(Some(&p), &liq_opp(), 8_000);
        // price update → borrow → begin → liquidate → swap → settle_and_return → repay → transfer
        assert!(matches!(plan[0], PtbStep::PriceUpdate { .. }));
        assert!(matches!(
            plan[1],
            PtbStep::FlashBorrow {
                amount: 200_000_000,
                ..
            }
        ));
        assert!(matches!(plan[2], PtbStep::Begin { .. }));
        assert!(matches!(
            plan[3],
            PtbStep::Liquidate {
                protocol: Protocol::Scallop,
                ..
            }
        ));
        assert!(matches!(plan[4], PtbStep::Swap { .. }));
        assert!(matches!(plan[5], PtbStep::SettleAndReturn));
        assert!(matches!(plan[6], PtbStep::FlashRepay { .. }));
        assert!(matches!(plan[7], PtbStep::TransferToSender));
        assert_eq!(plan.len(), 8);
        // THE ordering the oracle correctness depends on: update → liquidate → settle → repay
        let upd = plan
            .iter()
            .position(|s| matches!(s, PtbStep::PriceUpdate { .. }))
            .unwrap();
        let liq = plan
            .iter()
            .position(|s| matches!(s, PtbStep::Liquidate { .. }))
            .unwrap();
        let swap = plan
            .iter()
            .position(|s| matches!(s, PtbStep::Swap { .. }))
            .unwrap();
        let settle = plan
            .iter()
            .position(|s| matches!(s, PtbStep::SettleAndReturn))
            .unwrap();
        let repay = plan
            .iter()
            .position(|s| matches!(s, PtbStep::FlashRepay { .. }))
            .unwrap();
        assert!(upd < liq && liq < swap && swap < settle && settle < repay);
    }

    #[test]
    fn owned_liquidation_plan_has_no_flash_and_ends_at_settle() {
        let plan = liquidation_plan(None, &liq_opp(), 1);
        assert!(matches!(plan.first(), Some(PtbStep::PriceUpdate { .. })));
        assert!(matches!(plan[1], PtbStep::Begin { .. }));
        assert!(matches!(plan[2], PtbStep::Liquidate { .. }));
        assert!(matches!(plan.last(), Some(PtbStep::Settle)));
        assert!(!plan
            .iter()
            .any(|s| matches!(s, PtbStep::FlashBorrow { .. } | PtbStep::FlashRepay { .. })));
    }
}

// --- live assembly: plan -> sui_types ProgrammableTransaction ----------------
#[cfg(feature = "live")]
mod live {
    use super::PtbStep;
    use crate::flashloan::{scallop_pins, FlashStyle};
    use crate::types::Dex;
    use anyhow::{bail, Context, Result};
    use sui_types::base_types::{ObjectID, SuiAddress};
    use sui_types::programmable_transaction_builder::ProgrammableTransactionBuilder;
    use sui_types::transaction::{Argument, Command, ObjectArg, ProgrammableTransaction};
    use sui_types::{Identifier, TypeTag};

    /// Per-hop on-chain refs the builder needs (resolved from chain just before
    /// building — object versions change every checkpoint, so never cache these).
    ///
    /// The adapter argument order differs per venue (this is the Phase-2 "PTB builder
    /// changes"); `build` assembles each hop's args to match its adapter signature:
    ///   - AmmV2:  `swap_*<A,B>(pool, coin, min_out, ctx)`
    ///   - Cetus:  `swap_*<A,B>(config, pool, coin, min_out, clock, ctx)`
    ///   - Turbos: `swap_*<A,B,FeeType>(pool, coin, min_out, clock, versioned, ctx)`
    pub struct ResolvedHop {
        pub dex: Dex,
        /// True if collateral/token_in is the pool's `token_a` (selects swap fn direction).
        pub a_to_b: bool,
        /// Mutable shared pool object.
        pub pool: ObjectArg,
        /// `[token_a, token_b]` for V2/Cetus; `[token_a, token_b, fee_type]` for Turbos.
        pub type_args: Vec<TypeTag>,
        pub min_out: u64,
        /// Cetus `GlobalConfig` (shared, immutable). Required for `Dex::Cetus`.
        pub cetus_config: Option<ObjectArg>,
        /// `Clock` at `0x6` (shared, immutable). Required for Cetus + Turbos.
        pub clock: Option<ObjectArg>,
        /// Turbos `Versioned` (shared, immutable). Required for `Dex::Turbos`.
        pub turbos_versioned: Option<ObjectArg>,
    }

    /// Everything the live builder needs, resolved against the current chain state.
    pub struct BuildInputs {
        pub package: ObjectID,          // arbitrage_system package id
        pub provider_package: ObjectID, // lender's package (mock = our package; scallop = protocol)
        pub base_type: TypeTag,         // borrowed/base coin type
        pub lender: ObjectArg,          // mock: FlashLender vault; scallop: Market (both mutable)
        pub amount: u64,                // loan size
        pub min_profit: u64,
        pub hops: Vec<ResolvedHop>,
        pub sender: SuiAddress,
        /// Flash provider shape — selects the borrow/repay call convention.
        pub flash_style: crate::flashloan::FlashStyle,
        /// Scallop `Version` object (required for `FlashStyle::Scallop`; `None` for mock).
        pub version: Option<ObjectArg>,
        /// amount + flash fee; for Scallop we split exactly this off `proceeds` to repay
        /// (its `repay_flash_loan` consumes the coin and returns nothing).
        pub repay_total: u64,
    }

    fn id(s: &str) -> Result<Identifier> {
        Identifier::new(s) // already returns anyhow::Result<Identifier>
    }

    /// Assemble the flash-arb PTB. `plan` orders the steps; `inputs` carries the
    /// resolved refs. The two functions are kept in lock-step (same shape the plan
    /// describes); we drive off `inputs` and assert the plan matches.
    pub fn build(plan: &[PtbStep], inputs: BuildInputs) -> Result<ProgrammableTransaction> {
        let mut ptb = ProgrammableTransactionBuilder::new();
        let base = vec![inputs.base_type.clone()];

        // 1. flash borrow → (loan coin, receipt). Shape depends on the provider.
        let amount_arg = ptb.pure(inputs.amount)?;
        let (loan, frcpt) = match inputs.flash_style {
            FlashStyle::MockVault => {
                // arbitrage_system::flash::borrow<T>(lender, amount) -> (Coin<T>, FlashReceipt)
                let lender_arg = ptb.obj(inputs.lender)?;
                let b = ptb.command(Command::move_call(
                    inputs.provider_package,
                    id("flash")?,
                    id("borrow")?,
                    base.clone(),
                    vec![lender_arg, amount_arg],
                ));
                (nested(b, 0), nested(b, 1))
            }
            FlashStyle::Scallop => {
                // flash_loan::borrow_flash_loan<T>(version, market, amount) -> (Coin<T>, FlashLoan<T>)
                let version = ptb.obj(
                    inputs
                        .version
                        .context("scallop flash needs Version object")?,
                )?;
                let market = ptb.obj(inputs.lender)?;
                let b = ptb.command(Command::move_call(
                    inputs.provider_package,
                    id(scallop_pins::FLASH_MODULE)?,
                    id(scallop_pins::BORROW_FN)?,
                    base.clone(),
                    vec![version, market, amount_arg],
                ));
                (nested(b, 0), nested(b, 1))
            }
        };

        // 2. executor::begin<T>(loan, min_profit) -> (coin, ArbReceipt)
        let min_profit_arg = ptb.pure(inputs.min_profit)?;
        let begin = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("begin")?,
            base.clone(),
            vec![loan, min_profit_arg],
        ));
        let coin = nested(begin, 0);
        let arcpt = nested(begin, 1);

        // 3. per-hop swaps, threading the coin through (shared with the owned path).
        let coin = lower_swaps(&mut ptb, inputs.package, plan, &inputs.hops, coin)?;

        // 4. executor::settle_and_return<T>(receipt, coin) -> proceeds (PROFIT GATE)
        let proceeds = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("settle_and_return")?,
            base.clone(),
            vec![arcpt, coin],
        ));

        // 5. flash repay (REPAYMENT GATE) + return the profit. Shape depends on provider.
        let recipient = ptb.pure(inputs.sender)?;
        match inputs.flash_style {
            FlashStyle::MockVault => {
                // flash::repay<T>(lender, receipt, proceeds) -> change; keep the change.
                let lender_arg2 = ptb.obj(inputs.lender)?;
                let change = ptb.command(Command::move_call(
                    inputs.provider_package,
                    id("flash")?,
                    id("repay")?,
                    base,
                    vec![lender_arg2, frcpt, proceeds],
                ));
                ptb.command(Command::TransferObjects(vec![change], recipient));
            }
            FlashStyle::Scallop => {
                // Split exactly repay_total off proceeds; repay_flash_loan(version, market,
                // owed, loan) consumes the coin (returns nothing); transfer the remainder.
                let owed_amt = ptb.pure(inputs.repay_total)?;
                let owed = ptb.command(Command::SplitCoins(proceeds, vec![owed_amt]));
                let version = ptb.obj(
                    inputs
                        .version
                        .context("scallop flash needs Version object")?,
                )?;
                let market = ptb.obj(inputs.lender)?;
                ptb.command(Command::move_call(
                    inputs.provider_package,
                    id(scallop_pins::FLASH_MODULE)?,
                    id(scallop_pins::REPAY_FN)?,
                    base,
                    vec![version, market, nested(owed, 0), frcpt],
                ));
                ptb.command(Command::TransferObjects(vec![proceeds], recipient));
            }
        }

        Ok(ptb.finish())
    }

    fn nested(cmd: Argument, ix: u16) -> Argument {
        match cmd {
            Argument::Result(i) => Argument::NestedResult(i, ix),
            other => other,
        }
    }

    /// Thread the input coin through every `PtbStep::Swap`, emitting the adapter move call
    /// per hop in the exact arg order each venue expects. Shared by the flash and owned
    /// builders so the swaps are byte-for-byte identical between modes.
    fn lower_swaps(
        ptb: &mut ProgrammableTransactionBuilder,
        package: ObjectID,
        plan: &[PtbStep],
        hops: &[ResolvedHop],
        mut coin: Argument,
    ) -> Result<Argument> {
        for (step, hop) in plan
            .iter()
            .filter(|s| matches!(s, PtbStep::Swap { .. }))
            .zip(hops.iter())
        {
            let PtbStep::Swap {
                module, function, ..
            } = step
            else {
                bail!("plan/hop mismatch")
            };
            let pool_arg = ptb.obj(hop.pool)?;
            let min_out_arg = ptb.pure(hop.min_out)?;
            let args = match hop.dex {
                Dex::AmmV2 => vec![pool_arg, coin, min_out_arg],
                Dex::Cetus => {
                    let config =
                        ptb.obj(hop.cetus_config.context("cetus hop missing GlobalConfig")?)?;
                    let clock = ptb.obj(hop.clock.context("cetus hop missing Clock")?)?;
                    vec![config, pool_arg, coin, min_out_arg, clock]
                }
                Dex::Turbos => {
                    let clock = ptb.obj(hop.clock.context("turbos hop missing Clock")?)?;
                    let versioned = ptb.obj(
                        hop.turbos_versioned
                            .context("turbos hop missing Versioned")?,
                    )?;
                    vec![pool_arg, coin, min_out_arg, clock, versioned]
                }
            };
            let swap = ptb.command(Command::move_call(
                package,
                id(module)?,
                id(function)?,
                hop.type_args.clone(),
                args,
            ));
            coin = swap; // single-return (Coin<Out>)
        }
        Ok(coin)
    }

    /// Inputs for the owned-capital builder: no lender/flash fields — the input coin is split
    /// from the sender's own SUI (the gas coin), and `executor::settle` returns it to the
    /// sender (enforcing the profit gate). Same `ResolvedHop`s as the flash path.
    pub struct OwnedInputs {
        pub package: ObjectID,
        pub base_type: TypeTag,
        pub amount: u64,
        pub min_profit: u64,
        pub hops: Vec<ResolvedHop>,
    }

    /// Assemble the owned-capital PTB: split input off the gas coin → `executor::begin` →
    /// swaps → `executor::settle` (profit gate + transfer to the sender). No flash
    /// borrow/repay, no Scallop. Capital at risk = gas only (a bad fill reverts at `settle`).
    pub fn build_owned(plan: &[PtbStep], inputs: OwnedInputs) -> Result<ProgrammableTransaction> {
        let mut ptb = ProgrammableTransactionBuilder::new();
        let base = vec![inputs.base_type.clone()];

        // 1. Split the input amount off the gas coin (SUI is both gas and the traded asset).
        let amount_arg = ptb.pure(inputs.amount)?;
        let split = ptb.command(Command::SplitCoins(Argument::GasCoin, vec![amount_arg]));
        let input = nested(split, 0);

        // 2. executor::begin<T>(input, min_profit) -> (coin, ArbReceipt). Records initiator =
        //    tx sender + initial_amount = input value (the profit baseline).
        let min_profit_arg = ptb.pure(inputs.min_profit)?;
        let begin = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("begin")?,
            base.clone(),
            vec![input, min_profit_arg],
        ));
        let coin = nested(begin, 0);
        let arcpt = nested(begin, 1);

        // 3. swaps (identical lowering to the flash path).
        let coin = lower_swaps(&mut ptb, inputs.package, plan, &inputs.hops, coin)?;

        // 4. executor::settle<T>(receipt, coin): asserts final >= initial + min_profit, then
        //    transfers the output back to the sender. No flash repay.
        ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("settle")?,
            base,
            vec![arcpt, coin],
        ));
        Ok(ptb.finish())
    }

    /// Everything the live liquidation builder needs, resolved fresh against chain.
    /// Scallop-shaped (v1); other protocols add their own variant. The flash leg uses
    /// Scallop's `borrow_flash_loan`/`repay_flash_loan` (version + market), and the
    /// liquidate uses `protocol::liquidate::liquidate<Debt,Coll>`.
    pub struct LiquidationInputs {
        pub package: ObjectID,         // arbitrage_system (executor)
        pub scallop_package: ObjectID, // Scallop `protocol` (flash_loan + liquidate)
        pub debt_type: TypeTag,
        pub collateral_type: TypeTag,
        pub version: ObjectArg,
        pub obligation: ObjectArg, // shared, mutable
        pub market: ObjectArg,     // shared, mutable (flash + liquidate share it)
        pub registry: ObjectArg,   // CoinDecimalsRegistry (immutable)
        pub x_oracle: ObjectArg,   // XOracle — IMMUTABLE (verified: liquidate takes &XOracle)
        pub clock: ObjectArg,
        // --- VERIFIED Pyth accumulator price update (docs/scallop-liquidation-verified.md §4) ---
        pub wormhole_package: ObjectID, // wormhole pkg (vaa::parse_and_verify)
        pub pyth_package: ObjectID,     // pyth pkg (pyth + hot_potato_vector)
        pub wormhole_state: ObjectArg,  // immutable
        pub pyth_state: ObjectArg,      // immutable
        /// Wormhole VAA extracted from the Hermes accumulator (`oracle::extract_vaa_from_accumulator`).
        pub vaa_bytes: Vec<u8>,
        /// The Hermes accumulator (PNAU) blob consumed by `create_authenticated_price_infos_using_accumulator`.
        pub accumulator_msg: Vec<u8>,
        /// MIST split off the gas coin to pay each `update_single_price_feed` call.
        pub pyth_fee: u64,
        /// Pyth `PriceInfoObject`s to refresh (debt + collateral feeds), MUTABLE.
        pub price_infos: Vec<ObjectArg>,
        pub repay_amount: u64,
        /// repay_amount + flash fee, split off `proceeds` to repay the loan exactly.
        pub repay_total: u64,
        pub min_profit: u64,
        pub swap: ResolvedHop, // seized collateral → debt asset
        pub sender: SuiAddress,
    }

    /// Assemble the Scallop liquidation PTB (flash-funded):
    /// borrow_flash_loan → begin → liquidate → swap-back → settle_and_return →
    /// repay_flash_loan → transfer remainder. All existing gates enforced; capital
    /// at risk = gas only (a bad land reverts at `settle`/repay).
    pub fn build_liquidation(
        plan: &[PtbStep],
        inputs: LiquidationInputs,
    ) -> Result<ProgrammableTransaction> {
        // Sanity: the plan must be a liquidation plan with a flash borrow + liquidate.
        if !plan.iter().any(|s| matches!(s, PtbStep::Liquidate { .. })) {
            bail!("build_liquidation called with a non-liquidation plan");
        }
        let mut ptb = ProgrammableTransactionBuilder::new();
        let debt = vec![inputs.debt_type.clone()];

        // Shared refs resolved once (the builder dedups identical ObjectArgs into one input).
        let clock = ptb.obj(inputs.clock)?;
        let version = ptb.obj(inputs.version)?;
        let market = ptb.obj(inputs.market)?;

        // 0. VERIFIED Pyth accumulator price update (docs/scallop-liquidation-verified.md
        //    §3/§4; matches successful on-chain liquidation tx AYdhgWMq…):
        //      vaa::parse_and_verify(wormhole_state, vaa, clock)
        //      pyth::create_authenticated_price_infos_using_accumulator(pyth_state, acc, vaas, clock) -> HPV
        //      per feed: update_single_price_feed(pyth_state, HPV, price_info, fee, clock) -> HPV
        //      hot_potato_vector::destroy<PriceInfo>(HPV)
        //    This refreshes the Pyth PriceInfoObjects in-band; Scallop's `&XOracle` reads
        //    them during liquidate. (Replaces the prior x_oracle::price_update_request splice,
        //    which no successful on-chain liquidation uses — see doc §6.)
        let wormhole_state = ptb.obj(inputs.wormhole_state)?;
        let vaa_arg = ptb.pure(inputs.vaa_bytes.clone())?;
        let verified_vaas = ptb.command(Command::move_call(
            inputs.wormhole_package,
            id("vaa")?,
            id("parse_and_verify")?,
            vec![],
            vec![wormhole_state, vaa_arg, clock],
        ));
        let pyth_state = ptb.obj(inputs.pyth_state)?;
        let acc_arg = ptb.pure(inputs.accumulator_msg.clone())?;
        let mut hpv = ptb.command(Command::move_call(
            inputs.pyth_package,
            id("pyth")?,
            id("create_authenticated_price_infos_using_accumulator")?,
            vec![],
            vec![pyth_state, acc_arg, verified_vaas, clock],
        ));
        for price_info_obj in inputs.price_infos {
            let fee_amt = ptb.pure(inputs.pyth_fee)?;
            let fee = ptb.command(Command::SplitCoins(Argument::GasCoin, vec![fee_amt]));
            let price_info = ptb.obj(price_info_obj)?;
            hpv = ptb.command(Command::move_call(
                inputs.pyth_package,
                id("pyth")?,
                id("update_single_price_feed")?,
                vec![],
                vec![pyth_state, hpv, price_info, nested(fee, 0), clock],
            ));
        }
        let price_info_ty: TypeTag = format!("{}::price_info::PriceInfo", inputs.pyth_package)
            .parse()
            .context("PriceInfo type tag")?;
        ptb.command(Command::move_call(
            inputs.pyth_package,
            id("hot_potato_vector")?,
            id("destroy")?,
            vec![price_info_ty],
            vec![hpv],
        ));

        // 1. Scallop flash: borrow_flash_loan<Debt>(version, market, amount) -> (Coin, FlashLoan)
        let amount = ptb.pure(inputs.repay_amount)?;
        let borrow = ptb.command(Command::move_call(
            inputs.scallop_package,
            id(scallop_pins::FLASH_MODULE)?,
            id(scallop_pins::BORROW_FN)?,
            debt.clone(),
            vec![version, market, amount],
        ));
        let repay_coin = nested(borrow, 0);
        let loan = nested(borrow, 1);

        // 2. executor::begin<Debt>(repay, min_profit) -> (coin, ArbReceipt)
        let min_profit = ptb.pure(inputs.min_profit)?;
        let begin = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("begin")?,
            debt.clone(),
            vec![repay_coin, min_profit],
        ));
        let repay_coin = nested(begin, 0);
        let arcpt = nested(begin, 1);

        // 3. protocol::liquidate::liquidate<Debt,Coll>(version, obligation, market, repay,
        //    registry, x_oracle, clock) -> (remain debt, seized collateral). VERIFIED arg
        //    order (doc §1/§3).
        let obligation = ptb.obj(inputs.obligation)?;
        let registry = ptb.obj(inputs.registry)?;
        let x_oracle = ptb.obj(inputs.x_oracle)?;
        let liq = ptb.command(Command::move_call(
            inputs.scallop_package,
            id(scallop_pins::LIQUIDATE_MODULE)?,
            id(scallop_pins::LIQUIDATE_FN)?,
            vec![inputs.debt_type.clone(), inputs.collateral_type.clone()],
            vec![
                version, obligation, market, repay_coin, registry, x_oracle, clock,
            ],
        ));
        let remain = nested(liq, 0);
        let seized = nested(liq, 1);

        // 4. swap seized collateral → debt asset (existing adapter convention)
        let h = &inputs.swap;
        let pool_arg = ptb.obj(h.pool)?;
        let min_out = ptb.pure(h.min_out)?;
        let swap_args = match h.dex {
            Dex::AmmV2 => vec![pool_arg, seized, min_out],
            Dex::Cetus => {
                let cfg = ptb.obj(h.cetus_config.context("cetus hop missing GlobalConfig")?)?;
                let clk = ptb.obj(h.clock.context("cetus hop missing Clock")?)?;
                vec![cfg, pool_arg, seized, min_out, clk]
            }
            Dex::Turbos => {
                let clk = ptb.obj(h.clock.context("turbos hop missing Clock")?)?;
                let ver = ptb.obj(h.turbos_versioned.context("turbos hop missing Versioned")?)?;
                vec![pool_arg, seized, min_out, clk, ver]
            }
        };
        let (module, function) = (
            super::adapter_module(h.dex),
            if h.a_to_b {
                "swap_exact_in_a_to_b"
            } else {
                "swap_exact_in_b_to_a"
            },
        );
        let debt_from_swap = ptb.command(Command::move_call(
            inputs.package,
            id(module)?,
            id(function)?,
            h.type_args.clone(),
            swap_args,
        ));

        // 5. merge the liquidate's leftover debt (`remain`, ≈0) into the swap output
        ptb.command(Command::MergeCoins(debt_from_swap, vec![remain]));

        // 6. executor::settle_and_return<Debt>(receipt, proceeds) (PROFIT GATE)
        let proceeds = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("settle_and_return")?,
            debt.clone(),
            vec![arcpt, debt_from_swap],
        ));

        // 7. repay flash: split exactly `repay_total` off proceeds, repay_flash_loan
        //    (Scallop's repay consumes the coin + loan and returns nothing).
        let owed_amt = ptb.pure(inputs.repay_total)?;
        let owed = ptb.command(Command::SplitCoins(proceeds, vec![owed_amt]));
        ptb.command(Command::move_call(
            inputs.scallop_package,
            id(scallop_pins::FLASH_MODULE)?,
            id(scallop_pins::REPAY_FN)?,
            debt,
            vec![version, market, nested(owed, 0), loan],
        ));

        // 8. transfer the remainder (net profit) to the sender
        let recipient = ptb.pure(inputs.sender)?;
        ptb.command(Command::TransferObjects(vec![proceeds], recipient));

        Ok(ptb.finish())
    }
}

/// Structural-equivalence test: the assembled liquidation PTB must match the VERIFIED
/// on-chain Scallop liquidation (docs/scallop-liquidation-verified.md §3) command-for-command
/// for the Pyth update + liquidate + flash legs. This is the offline proof that the builder
/// reproduces a known-good transaction shape (a true live `dry_run` additionally needs a live
/// underwater obligation + fresh Hermes bytes — doc §8).
#[cfg(all(test, feature = "live"))]
mod live_tests {
    use super::live::{build_liquidation, LiquidationInputs, ResolvedHop};
    use super::PtbStep;
    use crate::scanner::Protocol;
    use crate::types::Dex;
    use sui_types::base_types::{ObjectID, SequenceNumber, SuiAddress};
    use sui_types::transaction::{Argument, Command, ObjectArg, SharedObjectMutability};
    use sui_types::TypeTag;

    fn oid(n: u64) -> ObjectID {
        ObjectID::from_hex_literal(&format!("0x{n:x}")).unwrap()
    }
    fn shared(n: u64, mutable: bool) -> ObjectArg {
        ObjectArg::SharedObject {
            id: oid(n),
            initial_shared_version: SequenceNumber::from_u64(1),
            mutability: if mutable {
                SharedObjectMutability::Mutable
            } else {
                SharedObjectMutability::Immutable
            },
        }
    }
    fn tt(s: &str) -> TypeTag {
        s.parse().unwrap()
    }

    /// (module, function) for each MoveCall, in order.
    fn move_calls(pt: &sui_types::transaction::ProgrammableTransaction) -> Vec<(String, String)> {
        pt.commands
            .iter()
            .filter_map(|c| match c {
                Command::MoveCall(m) => Some((m.module.to_string(), m.function.to_string())),
                _ => None,
            })
            .collect()
    }

    fn build() -> sui_types::transaction::ProgrammableTransaction {
        let debt = "0x2::usdc::USDC";
        let coll = "0x2::sui::SUI";
        let swap = ResolvedHop {
            dex: Dex::Cetus,
            a_to_b: true,
            pool: shared(100, true),
            type_args: vec![tt(coll), tt(debt)],
            min_out: 0,
            cetus_config: Some(shared(101, false)),
            clock: Some(shared(6, false)),
            turbos_versioned: None,
        };
        let inputs = LiquidationInputs {
            package: oid(1),
            scallop_package: oid(2),
            debt_type: tt(debt),
            collateral_type: tt(coll),
            version: shared(10, false),
            obligation: shared(11, true),
            market: shared(12, true),
            registry: shared(13, false),
            x_oracle: shared(14, false),
            clock: shared(6, false),
            wormhole_package: oid(20),
            pyth_package: oid(21),
            wormhole_state: shared(22, false),
            pyth_state: shared(23, false),
            vaa_bytes: vec![1, 0, 0, 0],
            accumulator_msg: vec![80, 78, 65, 85],
            pyth_fee: 1,
            price_infos: vec![shared(30, true), shared(31, true)],
            repay_amount: 1_000,
            repay_total: 1_003,
            min_profit: 1,
            swap,
            sender: SuiAddress::ZERO,
        };
        let plan = vec![PtbStep::Liquidate {
            protocol: Protocol::Scallop,
            obligation_id: "0x11".into(),
        }];
        build_liquidation(&plan, inputs).unwrap()
    }

    #[test]
    fn liquidation_ptb_matches_verified_onchain_shape() {
        let pt = build();
        let calls = move_calls(&pt);
        let names: Vec<(&str, &str)> = calls
            .iter()
            .map(|(m, f)| (m.as_str(), f.as_str()))
            .collect();
        // VERIFIED §3/§4 ordering (with our executor::begin/settle_and_return profit gate
        // wrapping the liquidate — that wrapper is ours, everything else is the on-chain shape).
        assert_eq!(
            names,
            vec![
                ("vaa", "parse_and_verify"),
                ("pyth", "create_authenticated_price_infos_using_accumulator"),
                ("pyth", "update_single_price_feed"), // debt feed
                ("pyth", "update_single_price_feed"), // collateral feed
                ("hot_potato_vector", "destroy"),
                ("flash_loan", "borrow_flash_loan"),
                ("executor", "begin"),
                ("liquidate", "liquidate"),
                ("cetus_adapter", "swap_exact_in_a_to_b"),
                ("executor", "settle_and_return"),
                ("flash_loan", "repay_flash_loan"),
            ]
        );
    }

    #[test]
    fn liquidate_and_pyth_calls_have_verified_arity() {
        let pt = build();
        for c in &pt.commands {
            if let Command::MoveCall(m) = c {
                match (m.module.as_str(), m.function.as_str()) {
                    // liquidate<Debt,Coll>(version, obligation, market, repay, registry, x_oracle, clock)
                    ("liquidate", "liquidate") => {
                        assert_eq!(m.type_arguments.len(), 2, "liquidate type args");
                        assert_eq!(m.arguments.len(), 7, "liquidate args");
                    }
                    ("vaa", "parse_and_verify") => assert_eq!(m.arguments.len(), 3),
                    ("pyth", "create_authenticated_price_infos_using_accumulator") => {
                        assert_eq!(m.arguments.len(), 4)
                    }
                    ("pyth", "update_single_price_feed") => assert_eq!(m.arguments.len(), 5),
                    ("hot_potato_vector", "destroy") => {
                        assert_eq!(m.type_arguments.len(), 1, "destroy<PriceInfo>")
                    }
                    _ => {}
                }
            }
        }
        // Each price feed's update fee is split off the GAS coin (verified: SplitCoins(Gas,[1])).
        let gas_splits = pt
            .commands
            .iter()
            .filter(|c| matches!(c, Command::SplitCoins(Argument::GasCoin, _)))
            .count();
        assert_eq!(gas_splits, 2, "one gas-funded fee split per feed");
    }
}

/// Adapter module name for a venue (shared by the live builders).
#[cfg(feature = "live")]
fn adapter_module(dex: Dex) -> &'static str {
    match dex {
        Dex::AmmV2 => "amm_v2_adapter",
        Dex::Cetus => "cetus_adapter",
        Dex::Turbos => "turbos_adapter",
    }
}

#[cfg(feature = "live")]
pub use live::{
    build, build_liquidation, build_owned, BuildInputs, LiquidationInputs, OwnedInputs, ResolvedHop,
};
