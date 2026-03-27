use cosmwasm_std::{Attribute, Event};
use cosmwasm_vm::AnalysisReport;
use layer_std::AccountId;

use super::WasmError;

const CONTRACT_ATTR: &str = "_contract_address";

/// This constructs events from a response as per
/// https://github.com/CosmWasm/wasmd/blob/main/EVENTS.md#standard-events-in-xwasm
pub(crate) fn build_contract_events(
    contract: &AccountId,
    events: Vec<Event>,
    attributes: Vec<Attribute>,
) -> Result<Vec<Event>, WasmError> {
    // All events get the _contract_addr prepended to attributes as well as
    let len = events.len() + boolean_one(!attributes.is_empty());
    let mut output = Vec::with_capacity(len);
    for ev in events {
        validate_attributes(&ev.attributes)?;
        output.push(
            Event::new(format!("wasm-{}", ev.ty))
                .add_attribute(CONTRACT_ATTR, contract.to_string())
                .add_attributes(ev.attributes),
        );
    }
    if !attributes.is_empty() {
        validate_attributes(&attributes)?;
        output.push(
            Event::new("wasm")
                .add_attribute(CONTRACT_ATTR, contract.to_string())
                .add_attributes(attributes),
        );
    }
    Ok(output)
}

/// LATER: Make generic over int types?
fn boolean_one(b: bool) -> usize {
    match b {
        true => 1,
        false => 0,
    }
}

fn validate_attributes(attrs: &[Attribute]) -> Result<(), WasmError> {
    for attr in attrs {
        if attr.key.starts_with('_') {
            return Err(WasmError::InvalidAttributeKey(attr.key.clone()));
        }
    }
    Ok(())
}

pub(crate) fn store_code_event(code_id: u64, analysis: AnalysisReport) -> Event {
    let mut evt = Event::new("store_code").add_attribute("code_id", code_id.to_string());
    for cap in analysis.required_capabilities.iter() {
        evt = evt.add_attribute("feature", cap);
    }
    evt
}

pub(crate) fn instantiate_event(contract: &AccountId, code_id: u64) -> Event {
    Event::new("instantiate")
        .add_attribute("code_id", code_id.to_string())
        .add_attribute(CONTRACT_ATTR, contract.to_string())
}

pub(crate) fn execute_event(contract: &AccountId) -> Event {
    Event::new("execute").add_attribute(CONTRACT_ATTR, contract.to_string())
}

#[allow(dead_code)]
pub(crate) fn migrate_event(contract: &AccountId, code_id: u64) -> Event {
    Event::new("migrate")
        .add_attribute("code_id", code_id.to_string())
        .add_attribute(CONTRACT_ATTR, contract.to_string())
}

pub(crate) fn update_admin_event(contract: &AccountId, admin: &AccountId) -> Event {
    Event::new("update_admin")
        .add_attribute(CONTRACT_ATTR, contract.to_string())
        .add_attribute("admin", admin.to_string())
}

pub(crate) fn clear_admin_event(contract: &AccountId) -> Event {
    Event::new("clear_admin").add_attribute(CONTRACT_ATTR, contract.to_string())
}

pub(crate) fn pin_code_event(code_id: u64) -> Event {
    Event::new("pin_code").add_attribute("code_id", code_id.to_string())
}

pub(crate) fn unpin_code_event(code_id: u64) -> Event {
    Event::new("unpin_code").add_attribute("code_id", code_id.to_string())
}

#[allow(dead_code)]
pub(crate) fn sudo_event(contract: &AccountId) -> Event {
    Event::new("sudo").add_attribute(CONTRACT_ATTR, contract.to_string())
}

#[allow(dead_code)]
pub(crate) fn reply_event(contract: &AccountId, is_success: bool) -> Event {
    let mode = match is_success {
        true => "handle_success",
        false => "handle_failure",
    };
    Event::new("reply")
        .add_attribute(CONTRACT_ATTR, contract.to_string())
        .add_attribute("mode", mode)
}
