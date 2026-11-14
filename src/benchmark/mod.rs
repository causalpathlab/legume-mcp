//! `legume benchmark`: a sweep of a grid of parameters times seeds times
//! stages and their methods, planned from a spec ([`spec`]): expanded to
//! its cells ([`plan`]), keyed by content ([`key`]), its tools resolved and
//! snapshot ([`tools`]), on a machine it describes ([`machine`]).

pub mod key;
pub mod machine;
pub mod plan;
pub mod spec;
pub mod tools;
