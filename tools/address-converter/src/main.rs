use std::env;

use alloy_primitives::Address;

// Usage: cargo run slay3r1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmvk3r3j

fn main() {
    let args: Vec<_> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: address-converter <bech32addr> ...");
        return;
    }
    for addr in &args[1..] {
        let (_, data) = bech32::decode(addr).unwrap();
        if data.len() != 20 {
            println!("{} isn't 20 bytes long", addr);
        }
        let addr = Address(data.as_slice().try_into().unwrap());
        println!("{}", addr);
    }
}
