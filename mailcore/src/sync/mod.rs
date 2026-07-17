//! The sync engine — drains the local outbox queue to Microsoft Graph, and
//! (a later task) runs delta sync to pull Graph changes into the store.

pub mod outbox;
