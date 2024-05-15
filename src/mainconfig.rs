use serde::{Serialize, Deserialize};


#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MainConfig {
    pub selected_device: Option<String>,
}

impl Default for MainConfig {
    fn default() -> Self {
        Self { selected_device: None }
    }
}
