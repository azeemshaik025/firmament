//! Circle Gateway reduced attestation parsing.

use solana_sdk::pubkey::Pubkey;

use super::{
    ATTESTATION_HEADER_SIZE, ELEM_DEST_RECIPIENT, ELEM_DEST_TOKEN, ELEM_FIXED_SIZE,
    ELEM_HOOK_DATA_LENGTH, ELEM_TRANSFER_SPEC_HASH, ELEM_VALUE, GatewayError, GatewayResult,
    NUM_ATTESTATIONS_OFFSET,
};

/// One decoded reduced Gateway mint attestation element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationElement {
    /// Destination token mint.
    pub dest_token: Pubkey,
    /// Destination recipient token account.
    pub dest_recipient: Pubkey,
    /// Raw mint amount.
    pub value: u64,
    /// `TransferSpec` hash used for replay protection.
    pub transfer_spec_hash: [u8; 32],
}

/// Parse all reduced Solana mint attestation elements.
///
/// # Errors
///
/// Returns an error when the payload is truncated or internally inconsistent.
pub fn parse_attestation_elements(attestation: &[u8]) -> GatewayResult<Vec<AttestationElement>> {
    if attestation.len() < ATTESTATION_HEADER_SIZE {
        return Err(GatewayError::InvalidAttestation(format!(
            "payload must be at least {ATTESTATION_HEADER_SIZE} bytes"
        )));
    }

    let count_bytes: [u8; 4] = attestation[NUM_ATTESTATIONS_OFFSET..ATTESTATION_HEADER_SIZE]
        .try_into()
        .map_err(|_| GatewayError::InvalidAttestation("missing attestation count".to_owned()))?;
    let count = u32::from_be_bytes(count_bytes) as usize;
    let max_possible_count = (attestation.len() - ATTESTATION_HEADER_SIZE) / ELEM_FIXED_SIZE;
    if count > max_possible_count {
        return Err(GatewayError::InvalidAttestation(format!(
            "header declares {count} attestations but payload can contain at most {max_possible_count}"
        )));
    }

    let mut cursor = ATTESTATION_HEADER_SIZE;
    let mut elements = Vec::with_capacity(count);
    for index in 0..count {
        if attestation.len() < cursor + ELEM_FIXED_SIZE {
            return Err(GatewayError::InvalidAttestation(format!(
                "attestation element {index} is truncated"
            )));
        }

        let element = &attestation[cursor..cursor + ELEM_FIXED_SIZE];
        let dest_token = Pubkey::try_from(&element[ELEM_DEST_TOKEN..ELEM_DEST_TOKEN + 32])
            .map_err(|error| GatewayError::InvalidAttestation(error.to_string()))?;
        let dest_recipient =
            Pubkey::try_from(&element[ELEM_DEST_RECIPIENT..ELEM_DEST_RECIPIENT + 32])
                .map_err(|error| GatewayError::InvalidAttestation(error.to_string()))?;

        let mut value_bytes = [0u8; 8];
        value_bytes.copy_from_slice(&element[ELEM_VALUE..ELEM_VALUE + 8]);
        let value = u64::from_be_bytes(value_bytes);

        let mut transfer_spec_hash = [0u8; 32];
        transfer_spec_hash
            .copy_from_slice(&element[ELEM_TRANSFER_SPEC_HASH..ELEM_TRANSFER_SPEC_HASH + 32]);

        let hook_data_len_bytes: [u8; 4] = element[ELEM_HOOK_DATA_LENGTH..ELEM_FIXED_SIZE]
            .try_into()
            .map_err(|_| GatewayError::InvalidAttestation("missing hook length".to_owned()))?;
        let hook_data_len = u32::from_be_bytes(hook_data_len_bytes) as usize;
        let total_len = ELEM_FIXED_SIZE.checked_add(hook_data_len).ok_or_else(|| {
            GatewayError::InvalidAttestation("hook data length overflow".to_owned())
        })?;
        let next_cursor = cursor.checked_add(total_len).ok_or_else(|| {
            GatewayError::InvalidAttestation("attestation cursor overflow".to_owned())
        })?;
        if next_cursor > attestation.len() {
            return Err(GatewayError::InvalidAttestation(format!(
                "attestation element {index} hook data is truncated"
            )));
        }

        elements.push(AttestationElement {
            dest_token,
            dest_recipient,
            value,
            transfer_spec_hash,
        });
        cursor = next_cursor;
    }

    Ok(elements)
}
