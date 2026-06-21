/// Cetus CLMM adapter (integration seam).
///
/// Cetus is a concentrated-liquidity DEX; its swap entrypoint differs from the
/// constant-product shape, so the adapter's job is to normalize it to the
/// convention in `amm_v2_adapter` (exact-in `Coin<In>` -> exact-out `Coin<Out>`).
///
/// HOW TO WIRE THIS UP
/// -------------------
/// 1. Add the Cetus interface package to `Move.toml` `[dependencies]` and pin the
///    rev/address for your target network (see the commented entry there).
/// 2. Replace the bodies below with a `flash_swap` + `repay_flash_swap` pair:
///
///        use cetus_clmm::pool::{Self as cetus_pool, Pool as CetusPool};
///        use cetus_clmm::config::GlobalConfig;
///
///        public fun swap_exact_in_a_to_b<A, B>(
///            config: &GlobalConfig,
///            pool: &mut CetusPool<A, B>,
///            coin_in: Coin<A>,
///            min_out: u64,
///            sqrt_price_limit: u128,
///            clock: &Clock,
///            ctx: &mut TxContext,
///        ): Coin<B> {
///            let amount_in = coin::value(&coin_in);
///            let (recv_a, recv_b, receipt) = cetus_pool::flash_swap<A, B>(
///                config, pool, /*a2b*/ true, /*by_amount_in*/ true,
///                amount_in, sqrt_price_limit, clock,
///            );
///            // pay `amount_in` of A, take B out, settle the flash receipt...
///            // assert!(coin::value(&out_b) >= min_out, E_SLIPPAGE);
///        }
///
/// Until wired, these abort so the package still compiles and the convention is
/// documented and type-checked. `abort` is divergent, so the unconsumed `Coin`
/// inputs are fine — the transaction reverts before they would leak.
module arbitrage_system::cetus_adapter;

use sui::coin::Coin;

/// Adapter has not been wired to the live Cetus package yet.
const E_ADAPTER_NOT_CONFIGURED: u64 = 1;

public fun swap_exact_in_a_to_b<A, B>(
    _coin_in: Coin<A>,
    _min_out: u64,
    _ctx: &mut TxContext,
): Coin<B> {
    abort E_ADAPTER_NOT_CONFIGURED
}

public fun swap_exact_in_b_to_a<A, B>(
    _coin_in: Coin<B>,
    _min_out: u64,
    _ctx: &mut TxContext,
): Coin<A> {
    abort E_ADAPTER_NOT_CONFIGURED
}
