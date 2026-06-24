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
    let mut steps = Vec::with_capacity(opp.route.len() + 5);
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
        // borrow → begin → liquidate → swap → settle_and_return → repay → transfer
        assert!(matches!(
            plan[0],
            PtbStep::FlashBorrow {
                amount: 200_000_000,
                ..
            }
        ));
        assert!(matches!(plan[1], PtbStep::Begin { .. }));
        assert!(matches!(
            plan[2],
            PtbStep::Liquidate {
                protocol: Protocol::Scallop,
                ..
            }
        ));
        assert!(matches!(plan[3], PtbStep::Swap { .. }));
        assert!(matches!(plan[4], PtbStep::SettleAndReturn));
        assert!(matches!(plan[5], PtbStep::FlashRepay { .. }));
        assert!(matches!(plan[6], PtbStep::TransferToSender));
        assert_eq!(plan.len(), 7);
        // liquidate precedes the swap-back which precedes the profit gate
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
        assert!(liq < swap && swap < settle);
    }

    #[test]
    fn owned_liquidation_plan_has_no_flash_and_ends_at_settle() {
        let plan = liquidation_plan(None, &liq_opp(), 1);
        assert!(matches!(plan.first(), Some(PtbStep::Begin { .. })));
        assert!(matches!(plan[1], PtbStep::Liquidate { .. }));
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
        pub provider_package: ObjectID, // lender's package (== package for the mock)
        pub base_type: TypeTag,         // borrowed/base coin type
        pub lender: ObjectArg,          // shared lender vault (mutable)
        pub amount: u64,                // loan size
        pub min_profit: u64,
        pub hops: Vec<ResolvedHop>,
        pub sender: SuiAddress,
    }

    fn id(s: &str) -> Result<Identifier> {
        Ok(Identifier::new(s)?)
    }

    /// Assemble the flash-arb PTB. `plan` orders the steps; `inputs` carries the
    /// resolved refs. The two functions are kept in lock-step (same shape the plan
    /// describes); we drive off `inputs` and assert the plan matches.
    pub fn build(plan: &[PtbStep], inputs: BuildInputs) -> Result<ProgrammableTransaction> {
        let mut ptb = ProgrammableTransactionBuilder::new();
        let base = vec![inputs.base_type.clone()];

        // 1. flash::borrow<T>(lender, amount) -> (loan, FlashReceipt)
        let lender_arg = ptb.obj(inputs.lender)?;
        let amount_arg = ptb.pure(inputs.amount)?;
        let borrow = ptb.command(Command::move_call(
            inputs.provider_package,
            id("flash")?,
            id("borrow")?,
            base.clone(),
            vec![lender_arg, amount_arg],
        ));
        let loan = nested(borrow, 0);
        let frcpt = nested(borrow, 1);

        // 2. executor::begin<T>(loan, min_profit) -> (coin, ArbReceipt)
        let min_profit_arg = ptb.pure(inputs.min_profit)?;
        let begin = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("begin")?,
            base.clone(),
            vec![loan, min_profit_arg],
        ));
        let mut coin = nested(begin, 0);
        let arcpt = nested(begin, 1);

        // 3. per-hop swaps, threading the coin through
        for (step, hop) in plan
            .iter()
            .filter(|s| matches!(s, PtbStep::Swap { .. }))
            .zip(inputs.hops.iter())
        {
            let PtbStep::Swap {
                module, function, ..
            } = step
            else {
                bail!("plan/hop mismatch")
            };
            let pool_arg = ptb.obj(hop.pool)?;
            let min_out_arg = ptb.pure(hop.min_out)?;
            // Assemble args in the exact order each adapter expects (ctx is implicit).
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
                inputs.package,
                id(module)?,
                id(function)?,
                hop.type_args.clone(),
                args,
            ));
            coin = swap; // single-return (Coin<Out>)
        }

        // 4. executor::settle_and_return<T>(receipt, coin) -> proceeds (PROFIT GATE)
        let proceeds = ptb.command(Command::move_call(
            inputs.package,
            id("executor")?,
            id("settle_and_return")?,
            base.clone(),
            vec![arcpt, coin],
        ));

        // 5. flash::repay<T>(lender, receipt, proceeds) -> change (REPAYMENT GATE)
        let lender_arg2 = ptb.obj(inputs.lender)?;
        let change = ptb.command(Command::move_call(
            inputs.provider_package,
            id("flash")?,
            id("repay")?,
            base,
            vec![lender_arg2, frcpt, proceeds],
        ));

        // 6. transfer the change (net profit) to the sender
        let recipient = ptb.pure(inputs.sender)?;
        ptb.command(Command::TransferObjects(vec![change], recipient));

        Ok(ptb.finish())
    }

    fn nested(cmd: Argument, ix: u16) -> Argument {
        match cmd {
            Argument::Result(i) => Argument::NestedResult(i, ix),
            other => other,
        }
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
        pub registry: ObjectArg,   // CoinDecimalsRegistry
        pub x_oracle: ObjectArg,
        pub clock: ObjectArg,
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

        // 1. Scallop flash: borrow_flash_loan<Debt>(version, market, amount) -> (Coin, FlashLoan)
        let version = ptb.obj(inputs.version)?;
        let market = ptb.obj(inputs.market)?;
        let amount = ptb.pure(inputs.repay_amount)?;
        let borrow = ptb.command(Command::move_call(
            inputs.scallop_package,
            id("flash_loan")?,
            id("borrow_flash_loan")?,
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
        //    registry, x_oracle, clock) -> (remain debt, seized collateral)
        let version3 = ptb.obj(inputs.version)?;
        let obligation = ptb.obj(inputs.obligation)?;
        let market3 = ptb.obj(inputs.market)?;
        let registry = ptb.obj(inputs.registry)?;
        let x_oracle = ptb.obj(inputs.x_oracle)?;
        let clock = ptb.obj(inputs.clock)?;
        let liq = ptb.command(Command::move_call(
            inputs.scallop_package,
            id("liquidate")?,
            id("liquidate")?,
            vec![inputs.debt_type.clone(), inputs.collateral_type.clone()],
            vec![
                version3, obligation, market3, repay_coin, registry, x_oracle, clock,
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
        let version7 = ptb.obj(inputs.version)?;
        let market7 = ptb.obj(inputs.market)?;
        ptb.command(Command::move_call(
            inputs.scallop_package,
            id("flash_loan")?,
            id("repay_flash_loan")?,
            debt,
            vec![version7, market7, nested(owed, 0), loan],
        ));

        // 8. transfer the remainder (net profit) to the sender
        let recipient = ptb.pure(inputs.sender)?;
        ptb.command(Command::TransferObjects(vec![proceeds], recipient));

        Ok(ptb.finish())
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
pub use live::{build, build_liquidation, BuildInputs, LiquidationInputs, ResolvedHop};
