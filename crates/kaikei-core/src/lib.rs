//! Phase 0: docs/01-core-types.md, docs/02-test-cases.md に基づき実装する。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod money;

pub use error::*;
pub use money::*;
