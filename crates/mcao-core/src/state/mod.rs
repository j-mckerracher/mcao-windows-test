//! Task identifiers, read-only `state/<id>.meta` records, their atomic publication and migration,
//! and the append-only status log.

pub mod encoding;
pub mod id;
pub(crate) mod lock;
pub mod meta;
pub mod migrate;
pub mod status;
pub mod store;

pub use encoding::{
    BackendToken, EncodingError, ExecutionTarget, FormatVersion, PathStyle, stamp_encoding,
};
pub use id::{TaskId, TaskIdError};
pub use meta::{AmbiguousKeyError, TaskMeta};
pub use migrate::{
    MigrateOutcome, RecordResults, RollbackOutcome, migrate_dir, migrate_meta, rollback_dir,
    rollback_meta,
};
pub use status::{ByteRange, OpenDecision, StatusKind, StatusLog};
pub use store::{publish_meta, publish_meta_encoded};
