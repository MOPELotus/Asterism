use aes::{
    Aes128,
    cipher::{Array, BlockCipherDecrypt, KeyInit},
};
use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use zeroize::Zeroize;

const ENCRYPTED_PREFIX: &str = "unipus.";
const AES_KEY_PREFIX: &[u8; 8] = b"1a2b3c4d";
const AES_BLOCK_BYTES: usize = 16;

pub(crate) fn decrypt_unipus_payload(
    encrypted: &str,
    key_suffix: &str,
    maximum_bytes: usize,
) -> ProviderResult<Vec<u8>> {
    let hexadecimal = encrypted
        .strip_prefix(ENCRYPTED_PREFIX)
        .ok_or_else(|| protocol_drift("UAI encrypted payload has an unknown prefix"))?;
    if maximum_bytes == 0
        || hexadecimal.is_empty()
        || !hexadecimal.len().is_multiple_of(AES_BLOCK_BYTES * 2)
        || hexadecimal.len() / 2 > maximum_bytes
    {
        return Err(invalid_response(
            "UAI encrypted payload has an invalid bounded length",
        ));
    }
    let suffix = key_suffix.as_bytes();
    if suffix.len() != 8 || !suffix.iter().all(u8::is_ascii_graphic) {
        return Err(protocol_drift(
            "UAI encrypted payload key suffix has an invalid shape",
        ));
    }
    let mut key = [0_u8; AES_BLOCK_BYTES];
    key[..AES_KEY_PREFIX.len()].copy_from_slice(AES_KEY_PREFIX);
    key[AES_KEY_PREFIX.len()..].copy_from_slice(suffix);
    let cipher = Aes128::new(&Array::from(key));
    key.zeroize();
    let mut decoded = decode_hex(hexadecimal)?;
    for chunk in decoded.chunks_exact_mut(AES_BLOCK_BYTES) {
        let mut block_bytes = [0_u8; AES_BLOCK_BYTES];
        block_bytes.copy_from_slice(chunk);
        let mut block = Array::from(block_bytes);
        block_bytes.zeroize();
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
        block.zeroize();
    }
    remove_padding(&mut decoded)?;
    if decoded.is_empty() || decoded.len() > maximum_bytes {
        decoded.zeroize();
        return Err(invalid_response(
            "UAI decrypted payload has an invalid bounded length",
        ));
    }
    Ok(decoded)
}

fn decode_hex(value: &str) -> ProviderResult<Vec<u8>> {
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])
            .ok_or_else(|| invalid_response("UAI encrypted payload is not hexadecimal"))?;
        let low = hex_nibble(pair[1])
            .ok_or_else(|| invalid_response("UAI encrypted payload is not hexadecimal"))?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn remove_padding(value: &mut Vec<u8>) -> ProviderResult<()> {
    let Some(&last) = value.last() else {
        return Err(invalid_response("UAI decrypted payload is empty"));
    };
    let padding = usize::from(last);
    if (1..=AES_BLOCK_BYTES).contains(&padding)
        && value.len() >= padding
        && value[value.len() - padding..]
            .iter()
            .all(|byte| usize::from(*byte) == padding)
    {
        value.truncate(value.len() - padding);
        return Ok(());
    }
    let original = value.len();
    while value.last() == Some(&0) {
        value.pop();
    }
    if value.len() == original {
        return Err(invalid_response(
            "UAI decrypted payload has invalid padding",
        ));
    }
    Ok(())
}

fn invalid_response(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidResponse, message)
}

fn protocol_drift(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::ProtocolDrift, message)
}
