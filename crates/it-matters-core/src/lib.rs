//! The plugin host core: maps plugin devices onto Matter units, allocates stable endpoint ids,
//! tracks device state and talks to the Matter side through a channel boundary. No Matter, no
//! sockets: those live in other crates, so everything here is testable with plain unit tests.

pub mod allocator;
pub mod frontend;
pub mod host;
pub mod mapping;
pub mod registry;
pub mod supervisor;
