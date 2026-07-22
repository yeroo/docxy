//! Shared File backstage (menu + folder browser + preview + Save As) and the
//! no-file start dialog, used by docxy/xlsxy/yppxy. The crate owns all state,
//! navigation, layout and rendering; each app supplies format-specific content
//! via [`BackstageHost`] and acts on the returned [`BackstageEvent`].
mod state;
pub use state::{Backstage, BackstageLayout, Entry, ITEMS, Item, Pane};
