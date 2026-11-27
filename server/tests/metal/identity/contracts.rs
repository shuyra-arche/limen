//! Type checks for the production table and its linear tokens.
//!
//! A single owner can cancel a reservation and release the resulting token.
//!
//! ```rust
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! let mut slots = [Entry::<()>::EMPTY];
//! let mut table = GenerationTable::new(1u32, &mut slots).unwrap();
//! let prepared = table.reserve().unwrap();
//! let pending = table.cancel(prepared).unwrap();
//! table.release(pending).unwrap();
//! ```
//!
//! Cancel consumes the original reservation.
//!
//! ```compile_fail,E0382
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! let mut slots = [Entry::<()>::EMPTY];
//! let mut table = GenerationTable::new(1u32, &mut slots).unwrap();
//! let prepared = table.reserve().unwrap();
//! let _ = table.cancel(prepared);
//! let _ = table.cancel(prepared);
//! ```
//!
//! A reservation cannot be cloned to create a second claim.
//!
//! ```compile_fail,E0277
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! let mut slots = [Entry::<()>::EMPTY];
//! let mut table = GenerationTable::new(1u32, &mut slots).unwrap();
//! let prepared = table.reserve().unwrap();
//! let _ = Clone::clone(&prepared);
//! ```
//!
//! Release consumes the original reclamation.
//!
//! ```compile_fail,E0382
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! let mut slots = [Entry::<()>::EMPTY];
//! let mut table = GenerationTable::new(1u32, &mut slots).unwrap();
//! let prepared = table.reserve().unwrap();
//! let pending = table.cancel(prepared).unwrap();
//! let _ = table.release(pending);
//! let _ = table.release(pending);
//! ```
//!
//! A reclamation cannot be cloned to create a second claim.
//!
//! ```compile_fail,E0277
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! let mut slots = [Entry::<()>::EMPTY];
//! let mut table = GenerationTable::new(1u32, &mut slots).unwrap();
//! let prepared = table.reserve().unwrap();
//! let pending = table.cancel(prepared).unwrap();
//! let _ = Clone::clone(&pending);
//! ```
//!
//! A table accepts tokens from its own scope type.
//!
//! ```compile_fail,E0308
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! #[derive(Clone, Copy, Debug, Eq, PartialEq)]
//! struct Allocation(u32);
//! #[derive(Clone, Copy, Debug, Eq, PartialEq)]
//! struct Return(u32);
//! let mut allocation_slots = [Entry::<()>::EMPTY];
//! let mut return_slots = [Entry::<()>::EMPTY];
//! let mut allocations = GenerationTable::new(Allocation(1), &mut allocation_slots).unwrap();
//! let mut returns = GenerationTable::new(Return(1), &mut return_slots).unwrap();
//! let prepared = allocations.reserve().unwrap();
//! let _ = returns.publish(prepared, ());
//! ```
//!
//! A live borrow prevents reclaiming the same table.
//!
//! ```compile_fail,E0502
//! # mod identity {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../src/metal/identity/mod.rs"));
//! # }
//! # use identity::treaty::{Entry, GenerationTable};
//! let mut slots = [Entry::<usize>::EMPTY];
//! let mut table = GenerationTable::new(1u32, &mut slots).unwrap();
//! let prepared = table.reserve().unwrap();
//! let key = table.publish(prepared, 17).unwrap();
//! let borrowed = table.get(key).unwrap();
//! let _ = table.reclaim(key);
//! assert_eq!(*borrowed, 17);
//! ```

#![no_std]
