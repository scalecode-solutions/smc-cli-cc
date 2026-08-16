pub mod emit;
pub mod records;

pub use emit::Emitter;
pub use records::{EndRecord, ErrorRecord, MetaRecord, SummaryRecord, SMC_TAG};
