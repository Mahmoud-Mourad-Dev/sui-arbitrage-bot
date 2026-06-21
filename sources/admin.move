/// Capability pattern. A single `AdminCap` is minted to the publisher at deploy
/// time. It gates privileged actions (e.g. creating/funding reference pools).
///
/// The arbitrage *executor* deliberately does NOT depend on this cap or any
/// shared config — execution is permissionless and stateless so it stays cheap
/// and free of shared-object contention. The cap exists only for operational
/// tooling and future extensions (fee sinks, pause switches on owned modules).
module arbitrage_system::admin;

/// Owned, transferable administrative capability.
public struct AdminCap has key, store {
    id: UID,
}

fun init(ctx: &mut TxContext) {
    transfer::public_transfer(AdminCap { id: object::new(ctx) }, ctx.sender());
}

#[test_only]
public fun mint_for_testing(ctx: &mut TxContext): AdminCap {
    AdminCap { id: object::new(ctx) }
}

#[test_only]
public fun burn_for_testing(cap: AdminCap) {
    let AdminCap { id } = cap;
    id.delete();
}
