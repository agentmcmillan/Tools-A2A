pub mod agent;
pub mod client;
pub mod convert;
pub mod hooks;
pub mod options;
pub mod types;

// Flatten the most-used types into the crate root for ergonomic imports.
pub use agent::{Agent, AgentBuilder};
pub use hooks::{TaskAssignedEvent, SnapshotChangedEvent, LifecycleEvent, BroadcastEvent};
pub use options::CallOptions;
pub use types::{Capability, Message, Response, Soul, StreamChunk, TodoItem};

