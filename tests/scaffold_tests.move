#[test_only]
module sui_arbitrage_bot::scaffold_tests;

use sui_arbitrage_bot::scaffold;
use sui::test_scenario;
use std::string;

const USER: address = @0xA;

#[test]
fun test_mint_and_read() {
    let mut scenario = test_scenario::begin(USER);

    // Mint a Greeting owned by USER.
    scaffold::mint(b"gm sui", scenario.ctx());

    // Move to the next tx so the minted object is available to take.
    scenario.next_tx(USER);

    let greeting = scenario.take_from_sender<scaffold::Greeting>();
    assert!(scaffold::message(&greeting) == string::utf8(b"gm sui"), 0);
    scenario.return_to_sender(greeting);

    scenario.end();
}
