// This file is meant to add extra functionality to the generated proto structs.

use layer_std::stringify_or_hex;

use crate::protos::layer::sync::v1::{state_change::Event, BlockWrites, StateChange, WriteData};

impl std::fmt::Display for WriteData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = stringify_or_hex(&self.value);
        write!(
            f,
            r#"WriteData {{ module: "{}", bucket: "{}", keys: {:?}, value: {} }}"#,
            self.module, self.bucket, self.keys, value
        )
    }
}

impl std::fmt::Display for BlockWrites {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BlockWrites( {} )", self.sequence)?;
        for evt in self.events.iter() {
            write!(f, "\n{}", evt)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for StateChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.event {
            Some(Event::WriteState(data)) => {
                let value = stringify_or_hex(&data.value);
                write!(
                    f,
                    r#"  Write: module: "{}", bucket: "{}", keys: {:?}, value: {}"#,
                    data.module, data.bucket, data.keys, value
                )
            }
            Some(Event::DeleteState(data)) => {
                write!(
                    f,
                    r#"  Delete: module: "{}", bucket: "{}", keys: {:?}"#,
                    data.module, data.bucket, data.keys
                )
            }
            // silently ignore case which should never happen
            None => Ok(()),
        }
    }
}
