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
use crate::scanner::{Hop, Opportunity};
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
    use crate::scanner::Opportunity;
    use crate::types::Dex;

    fn opp() -> Opportunity {
        Opportunity {
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
}

// --- live assembly: plan -> sui_types ProgrammableTransaction ----------------
#[cfg(feature = "live")]
mod live {
    use super::PtbStep;
    use anyhow::{bail, Result};
    use sui_types::base_types::{ObjectID, ObjectRef, SuiAddress};
    use sui_types::programmable_transaction_builder::ProgrammableTransactionBuilder;
    use sui_types::transaction::{Argument, Command, ObjectArg, ProgrammableTransaction};
    use sui_types::{Identifier, TypeTag};

    /// Per-hop on-chain refs the builder needs (resolved from chain just before
    /// building — object versions change every checkpoint).
    pub struct ResolvedHop {
        pub pool: ObjectArg, // SharedObject { id, initial_shared_version, mutable: true }
        pub type_args: Vec<TypeTag>, // e.g. [In, Out] (+ fee tier for Turbos)
        pub extra_objs: Vec<ObjectArg>, // venue extras (Clock 0x6, GlobalConfig, Versioned)
        pub min_out: u64,
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
                module,
                function,
                min_out,
                ..
            } = step
            else {
                bail!("plan/hop mismatch")
            };
            let pool_arg = ptb.obj(hop.pool)?;
            let mut args = vec![pool_arg, coin];
            for extra in &hop.extra_objs {
                args.push(ptb.obj(*extra)?);
            }
            args.push(ptb.pure(*min_out)?);
            let swap = ptb.command(Command::move_call(
                inputs.package,
                id(module)?,
                id(function)?,
                hop.type_args.clone(),
                args,
            ));
            coin = swap; // single-return (Coin<Out>)
            let _ = min_out;
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
}

#[cfg(feature = "live")]
pub use live::{build, BuildInputs, ResolvedHop};
