use std::fmt::{self, Display, Formatter};

// This is measured in bytes, twice as long in hex
// point is to not dump entire wasm blobs...
const MAX_DISPLAY_LENGTH: usize = 1000;
pub struct HexEncode<'a>(&'a [u8]);

impl<'a> HexEncode<'a> {
    pub fn new(bytes: &'a impl AsRef<[u8]>) -> HexEncode<'a> {
        HexEncode(bytes.as_ref())
    }
}

impl Display for HexEncode<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.0.len() > MAX_DISPLAY_LENGTH {
            write!(f, "{}...", hex::encode_upper(&self.0[..MAX_DISPLAY_LENGTH]))
        } else {
            f.write_str(&hex::encode_upper(self.0))
        }
    }
}

// TODO: check if we can just use Display on &[Coin] as the default Coin.Display is good
pub struct CoinEncode<'a>(pub &'a [cosmwasm_std::Coin]);

impl Display for CoinEncode<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for coin in self.0 {
            write!(f, "{}{},", coin.amount, coin.denom)?;
        }
        Ok(())
    }
}
