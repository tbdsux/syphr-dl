//! Links handed to Syphr from outside its window, such as a clipboard notification.

use serde::{Deserialize, Serialize};

/// Downloads to add, with how to add them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AddRequest {
    pub urls: Vec<String>,
    /// Save folder; None uses the one in Settings.
    pub dir: Option<String>,
    /// File name, for a single link.
    pub name: Option<String>,
    /// Add without showing the panel.
    pub silent: bool,
    /// Add paused instead of starting; implies `silent`.
    pub queue_only: bool,
}
