use crate::AccountId;

pub fn stringify_or_hex(input: &[u8]) -> String {
    std::str::from_utf8(input).map_or_else(|_| hex::encode_upper(input), |x| x.to_string())
}

/// Use when you expect binary AccountId. It tries:
/// 1) valid utf-8 string
/// 2) valid AccountId bytes
/// 3) hex encode
pub fn string_account_or_hex(input: &[u8]) -> String {
    match std::str::from_utf8(input) {
        Ok(s) => s.to_string(),
        Err(_) => match AccountId::new(input) {
            Ok(id) => id.to_string(),
            Err(_) => hex::encode_upper(input),
        },
    }
}
