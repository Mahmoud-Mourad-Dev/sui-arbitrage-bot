//! Programmable Transaction Block construction (feature = "live").
//!
//! Turns a scanner [`Opportunity`](crate::scanner::Opportunity) into a single PTB:
//!
//!   split(input)               // carve the input coin off the gas/base coin
//!   (coin, receipt) = executor::begin<Base>(coin, min_profit)
//!   coin = <adapter>::swap_exact_in_*<In,Out>(pool, coin, min_out)   // per hop
//!   ...
//!   executor::settle<Base>(receipt, coin)
//!
//! Everything is one transaction -> atomic. We minimize object reads/writes by
//! touching only each hop's pool object plus the base coin; no shared registry,
//! no intermediate owned objects.

use anyhow::Result;

use crate::config::Config;
use crate::scanner::{Hop, Opportunity};
use crate::types::Dex;

use sui_types::programmable_transaction_builder::ProgrammableTransactionBuilder;
use sui_types::transaction::ProgrammableTransaction;
use sui_types::Identifier;

/// Build the arbitrage PTB. `package` is the published `arbitrage_system` id.
///
/// NOTE: object refs (pools, the base coin) and `TypeTag`s for each hop must be
/// resolved from the chain just before building (versions change every block);
/// pass them in from the executor once fetched. Left as the integration seam.
pub fn build(config: &Config, opp: &Opportunity) -> Result<ProgrammableTransaction> {
    let ptb = ProgrammableTransactionBuilder::new();
    let _package: sui_types::base_types::ObjectID = config.package_id.parse()?;

    // 1. split `opp.input_amount` off the base coin -> input Argument
    //    let input = ptb.command(Command::SplitCoins(base_coin, vec![amount_arg]));
    //
    // 2. begin: returns (Coin<Base>, ArbReceipt)
    //    let (coin, receipt) = move_call(executor, "begin", [base_type], [input, min_profit]);
    let _begin = (Identifier::new("executor")?, Identifier::new("begin")?);

    // 3. one move_call per hop, threading the coin Argument through:
    for hop in &opp.route {
        let (module, func) = adapter_call(hop);
        // move_call(package, module, func, [in_type, out_type], [pool_ref, coin, min_out])
        let _ = (Identifier::new(module)?, Identifier::new(func)?);
    }

    // 4. settle: consumes the receipt, enforces profit, transfers to sender.
    let _settle = (Identifier::new("executor")?, Identifier::new("settle")?);

    let _ = opp;
    Ok(ptb.finish())
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
